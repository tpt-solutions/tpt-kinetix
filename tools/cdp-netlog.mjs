// Logs every request's start, response and finish, with times, from a Chrome that was started with
// --remote-debugging-port. Used to find which playlist or part request a browser run stalls on:
//   CDP_PORT=9411 tools/live-subtitle-browser-test.sh   (in another shell: node tools/cdp-netlog.mjs 9411 out.txt 30)
// Attaches to the page of a Chrome started with --remote-debugging-port and logs
// every request's start, response and finish with times relative to the first one.
const port = process.argv[2], out = process.argv[3], secs = Number(process.argv[4] || 20);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let targets;
for (let i = 0; i < 1000 && !targets; i++) {
  try { targets = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).filter((t) => t.type === "page" && t.url.includes("127.0.0.1")); } catch {}
  if (!targets || !targets.length) { targets = undefined; await sleep(100); }
}
const ws = new WebSocket(targets[0].webSocketDebuggerUrl);
const lines = []; const t0 = { v: null }; const at = (ts) => { t0.v ??= ts; return ((ts - t0.v) * 1000).toFixed(0).padStart(7); };
const names = new Map();
ws.onmessage = (ev) => {
  const m = JSON.parse(ev.data);
  if (!m.method) return;
  const p = m.params;
  if (m.method === "Network.requestWillBeSent") { names.set(p.requestId, p.request.url.replace(/^https?:\/\/[^/]+/, "")); lines.push(`${at(p.timestamp)} START ${names.get(p.requestId)}`); }
  if (m.method === "Network.responseReceived") lines.push(`${at(p.timestamp)} RESP  ${p.response.status} ${names.get(p.requestId)}`);
  if (m.method === "Network.loadingFinished") lines.push(`${at(p.timestamp)} DONE  ${names.get(p.requestId)} ${p.encodedDataLength}B`);
  if (m.method === "Network.loadingFailed") lines.push(`${at(p.timestamp)} FAIL  ${names.get(p.requestId)} ${p.errorText}`);
};
await new Promise((r) => (ws.onopen = r));
ws.send(JSON.stringify({ id: 1, method: "Network.enable", params: {} }));
await sleep(secs * 1000);
const fs = await import("node:fs");
fs.writeFileSync(out, lines.join("\n") + "\n");
ws.close();
