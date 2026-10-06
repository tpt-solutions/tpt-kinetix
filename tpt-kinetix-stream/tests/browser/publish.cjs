// Publishes from a real browser: headless Chrome opens the server's /publish page
// with a fake camera + microphone, MediaRecorder records AV1/VP9 + Opus WebM and
// streams it over a WebSocket. The test then requires the live HLS to appear with
// several segments and the segments to decode in ffmpeg.
//
//   node publish.cjs <chrome> <base-url> [key] [token]
//
// `base-url` is a running live server (`cargo run -p tpt-kinetix-stream --example live_server`).
const http = require("http");
const os = require("os");
const fs = require("fs");
const path = require("path");
const { spawn, spawnSync } = require("child_process");

const [chrome, base, key = "browsercam", token = ""] = process.argv.slice(2);
if (!chrome || !base) {
  console.error("usage: node publish.cjs <chrome> <base-url> [key] [token]");
  process.exit(2);
}

const get = (p) =>
  new Promise((resolve, reject) => {
    http.get(base + p, (res) => {
      const chunks = [];
      res.on("data", (c) => chunks.push(c));
      res.on("end", () => resolve({ status: res.statusCode, body: Buffer.concat(chunks) }));
    }).on("error", reject);
  });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

(async () => {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), "tpt-publish-"));
  const url = `${base}/publish?auto=1&key=${key}` + (token ? `&token=${token}` : "");
  const proc = spawn(chrome, [
    "--headless=new", "--disable-gpu", "--no-first-run", `--user-data-dir=${profile}`,
    "--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream",
    "--autoplay-policy=no-user-gesture-required", url,
  ], { stdio: "ignore" });
  let failure = null;
  try {
    let playlist = "";
    let track = 0;
    const deadline = Date.now() + 40000;
    while (Date.now() < deadline) {
      // The master names the video playlist after its STREAM-INF; the browser
      // decides the track order, so do not assume the video is track 0.
      const master = await get(`/${key}/master.m3u8`).catch(() => ({ status: 0 }));
      if (master.status !== 200) { await sleep(500); continue; }
      const lines = master.body.toString().split("\n");
      const inf = lines.findIndex((l) => l.startsWith("#EXT-X-STREAM-INF"));
      track = Number((lines[inf + 1] || "").match(/track-(\d+)/)?.[1] ?? 0);
      const r = await get(`/${key}/track-${track}.m3u8`).catch(() => ({ status: 0 }));
      if (r.status === 200) {
        playlist = r.body.toString();
        if ((playlist.match(/#EXTINF/g) || []).length >= 3) break;
      }
      await sleep(500);
    }
    const segs = playlist.split("\n").filter((l) => l.startsWith("seg-"));
    if (segs.length < 3) throw new Error(`only ${segs.length} segments appeared:\n${playlist}`);
    if (playlist.includes("#EXT-X-ENDLIST")) throw new Error("playlist ended while the browser is still publishing");
    const m = (await get("/metrics")).body.toString();
    if (!/kinetix_publishers_active 1/.test(m)) throw new Error("server does not show an active publisher:\n" + m);

    // The segments the browser produced must decode.
    const init = (await get(`/${key}/init-${track}.mp4`)).body;
    const parts = [init];
    for (const s of segs.slice(0, 3)) parts.push((await get(`/${key}/${s}`)).body);
    const file = path.join(profile, "out.mp4");
    fs.writeFileSync(file, Buffer.concat(parts));
    const ff = spawnSync(process.env.FFMPEG || "ffmpeg", ["-v", "error", "-i", file, "-map", "0:v:0", "-f", "framemd5", "-"], { encoding: "utf8" });
    if (ff.error) throw new Error(`cannot run ffmpeg (set FFMPEG=/path/to/ffmpeg): ${ff.error.message}`);
    const frames = (ff.stdout.match(/^0,/gm) || []).length;
    if (ff.status !== 0 || ff.stderr.trim() || frames < 10) {
      throw new Error(`recorded video does not decode cleanly (frames=${frames}): ${ff.stderr}`);
    }
    console.log(`browser publish OK: ${segs.length} segments live, ${frames} frames decoded from the first 3`);
  } catch (e) {
    failure = e;
  } finally {
    proc.kill();
    await sleep(500);
    try { fs.rmSync(profile, { recursive: true, force: true }); } catch {}
  }
  if (failure) { console.error("FAIL:", failure.message); process.exit(1); }
})();
