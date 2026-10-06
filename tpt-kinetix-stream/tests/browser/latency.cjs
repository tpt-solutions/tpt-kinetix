// Measures true glass-to-glass latency of the live HLS path in headless Chrome.
//
//   node latency.cjs <chrome> <libs-dir (hls.js)> <live-server-base-url> <ll:0|1> [seconds] [mime]
//
// The page (latency.html) publishes a wall-clock barcode through MediaRecorder +
// WebSocket and decodes it from the hls.js-played video, so the number covers
// capture, encode, ingest, packaging, HTTP and decode. Prints one JSON line.
const http = require("http");
const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawn } = require("child_process");

const [chrome, libs, live, ll = "1", secs = "30", mime = "video/webm;codecs=vp9,opus"] = process.argv.slice(2);
const key = process.env.KEY || "lat" + Math.random().toString(36).slice(2, 8);
const external = process.env.EXTERNAL === "1";

const server = http.createServer((req, res) => {
  const route = req.url.split("?")[0];
  if (req.method === "POST" && route === "/result") {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => { res.end("ok"); done(JSON.parse(body)); });
    return;
  }
  const file = route === "/page.html" ? path.join(__dirname, "latency.html")
    : route === "/hls.js" ? path.join(libs, "hls.js") : route === "/dash.js" ? path.join(libs, "dash.js") : null;
  if (!file) { res.statusCode = 404; return res.end(); }
  res.setHeader("Content-Type", route.endsWith(".js") ? "text/javascript" : "text/html");
  res.end(fs.readFileSync(file));
});

const profile = fs.mkdtempSync(path.join(os.tmpdir(), "tpt-lat-"));
let proc;
function pct(a, p) { return a[Math.min(a.length - 1, Math.floor(p * a.length))]; }
function done(r) {
  try { proc.kill(); } catch {}
  server.close();
  setTimeout(() => { try { fs.rmSync(profile, { recursive: true, force: true }); } catch {} }, 1500);
  const s = (r.samples || []).slice().sort((a, b) => a - b);
  const p = (r.playerLatency || []).slice().sort((a, b) => a - b);
  const out = {
    ll: ll === "1", mime, samples: s.length, errors: r.errors,
    glassToGlassMs: s.length ? { min: s[0], p50: pct(s, 0.5), p95: pct(s, 0.95), max: s[s.length - 1] } : null,
    video: { currentTime: r.currentTime, readyState: r.readyState, buffered: [r.bufferedStart, r.bufferedEnd], decodedFrames: r.decodedFrames, hlsEvents: r.hlsEvents, publisherGapsMs: r.chunkGaps, waits: r.waits, wsMaxBufferedBytes: r.maxBuffered },
    hlsjsLatencyMs: p.length ? { p50: Math.round(pct(p, 0.5)) } : null,
  };
  console.log(JSON.stringify(out));
  process.exit(external ? ((r.errors || []).length === 0 ? 0 : 1) : s.length >= 20 && (r.errors || []).length === 0 ? 0 : 1);
}

server.listen(0, "127.0.0.1", () => {
  const port = server.address().port;
  const page = `http://127.0.0.1:${port}/page.html?live=${encodeURIComponent(live)}&key=${key}&ll=${ll}&seconds=${secs}&mime=${encodeURIComponent(mime)}${external ? "&external=1" : ""}${process.env.PLAYER ? "&player=" + process.env.PLAYER : ""}${process.env.RATE ? "&rate=" + process.env.RATE : ""}${process.env.DEBUG_HLS ? "&debug=1" : ""}${process.env.SYNC ? "&sync=" + process.env.SYNC : ""}`;
  proc = spawn(chrome, [
    "--headless=new", "--disable-gpu", "--no-sandbox", "--mute-audio",
    "--autoplay-policy=no-user-gesture-required", `--user-data-dir=${profile}`,
    ...(process.env.CHROME_LOG ? ["--enable-logging=stderr", "--v=0"] : []), page,
  ], { stdio: ["ignore", "ignore", process.env.CHROME_LOG ? "inherit" : "ignore"] });
  setTimeout(() => done({ samples: [], errors: ["harness timeout"] }), (Number(secs) + 60) * 1000);
});
