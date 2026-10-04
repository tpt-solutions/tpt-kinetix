// Plays the packager's HLS and DASH output in headless Chrome with hls.js and
// dash.js (real MSE players) and requires playback to advance past a segment
// boundary with video decoded and no player errors.
//
//   node run.cjs <chrome.exe> <libs-dir (hls.js, dash.js)> <stream-base-url> [seconds]
//
// `stream-base-url` is a running `tpt-kinetix serve` (CORS is enabled there).
const http = require("http");
const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawn } = require("child_process");

const [chrome, libs, base, secs = "5"] = process.argv.slice(2);

function runOne(kind, url) {
  return new Promise((resolve) => {
    const server = http.createServer((req, res) => {
      if (req.method === "POST" && req.url === "/result") {
        let body = "";
        req.on("data", (c) => (body += c));
        req.on("end", () => {
          res.end("ok");
          cleanup();
          resolve(JSON.parse(body));
        });
        return;
      }
      const route = req.url.split("?")[0];
      const file =
        route === "/page.html" ? path.join(__dirname, "page.html")
        : route === "/hls.js" ? path.join(libs, "hls.js")
        : route === "/dash.js" ? path.join(libs, "dash.js")
        : null;
      if (!file) { res.statusCode = 404; return res.end(); }
      res.setHeader("Content-Type", route.endsWith(".js") ? "text/javascript" : "text/html");
      res.end(fs.readFileSync(file));
    });
    let proc;
    const profile = fs.mkdtempSync(path.join(os.tmpdir(), "tpt-chrome-"));
    function cleanup() {
      try { proc && proc.kill(); } catch {}
      server.close();
      setTimeout(() => { try { fs.rmSync(profile, { recursive: true, force: true }); } catch {} }, 1500);
    }
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      const page = `http://127.0.0.1:${port}/page.html?kind=${kind}&seconds=${secs}&url=${encodeURIComponent(url)}`;
      proc = spawn(chrome, [
        "--headless=new", "--disable-gpu", "--no-sandbox", "--mute-audio",
        "--autoplay-policy=no-user-gesture-required",
        `--user-data-dir=${profile}`, page,
      ], { stdio: "ignore" });
      setTimeout(() => { cleanup(); resolve({ ok: false, errors: ["harness timeout"] }); }, 60000);
    });
  });
}

(async () => {
  let failed = false;
  for (const [kind, file] of [["hls", "master.m3u8"], ["dash", "manifest.mpd"]]) {
    const r = await runOne(kind, `${base}/${file}`);
    const good = r.ok && r.videoWidth > 0 && (r.errors || []).length === 0 && (r.decodedFrames || 0) > 20;
    console.log(
      `${good ? "PASS" : "FAIL"} ${kind}: played to ${r.currentTime && r.currentTime.toFixed(2)}s of ` +
        `${r.duration && r.duration.toFixed ? r.duration.toFixed(2) : r.duration}s, ` +
        `${r.videoWidth}x${r.videoHeight}, decoded ${r.decodedFrames} frames (${r.droppedFrames} dropped), ` +
        `errors: ${JSON.stringify(r.errors)}`
    );
    if (!good) failed = true;
  }
  process.exit(failed ? 1 : 0);
})();
