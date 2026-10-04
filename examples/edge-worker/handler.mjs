// Just-in-time HLS/DASH packaging in a Worker (Cloudflare Workers, Deno Deploy,
// Fastly Compute, or Node 18+): the MP4 stays in object storage; only its index
// and the requested segment's byte ranges are fetched.
//
//   GET /<object-key>/master.m3u8
//   GET /<object-key>/track-0.m3u8   init-0.mp4   seg-0-3.m4s   manifest.mpd
//
// Origin is either an R2-style binding (`env.BUCKET`, with `get(key, {range})`
// and `head(key)`) or plain HTTP range requests against `env.ORIGIN_BASE/<key>`.

const SEGMENT_SECONDS = 6;

class HttpError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

/** Builds the fetch handler from the wasm-bindgen glue and the compiled module. */
export function createHandler({ WasmPackager, initSync, wasmModule }) {
  let ready = false;
  // One packager per object per isolate: the index is a few KB and immutable.
  const packagers = new Map();

  function originFor(key, env) {
    if (env.BUCKET) {
      return {
        async length() {
          const head = await env.BUCKET.head(key);
          if (!head) throw new HttpError(404, `no such object: ${key}`);
          return { size: head.size, etag: head.httpEtag || head.etag };
        },
        async read(offset, length) {
          const obj = await env.BUCKET.get(key, { range: { offset, length } });
          if (!obj) throw new HttpError(404, `no such object: ${key}`);
          return new Uint8Array(await obj.arrayBuffer());
        },
      };
    }
    const url = `${env.ORIGIN_BASE}/${key.split("/").map(encodeURIComponent).join("/")}`;
    return {
      async length() {
        // A 1-byte range request reveals the size without needing HEAD (which
        // pre-signed URLs often forbid).
        const r = await fetch(url, { headers: { Range: "bytes=0-0" } });
        if (r.status === 404) throw new HttpError(404, `no such object: ${key}`);
        const total = /\/(\d+)$/.exec(r.headers.get("content-range") || "");
        if (r.status !== 206 || !total) {
          throw new HttpError(502, "origin does not support range requests");
        }
        await r.arrayBuffer();
        return { size: Number(total[1]), etag: r.headers.get("etag") };
      },
      async read(offset, length) {
        const r = await fetch(url, {
          headers: { Range: `bytes=${offset}-${offset + length - 1}` },
        });
        if (r.status !== 206) {
          throw new HttpError(502, `origin returned ${r.status} for a range request`);
        }
        return new Uint8Array(await r.arrayBuffer());
      },
    };
  }

  async function packagerFor(key, env) {
    if (!ready) {
      initSync({ module: wasmModule });
      ready = true;
    }
    if (!packagers.has(key)) {
      const p = (async () => {
        const origin = originFor(key, env);
        const { size, etag } = await origin.length();
        const pk = await WasmPackager.open(size, (o, l) => origin.read(o, l), SEGMENT_SECONDS);
        return { pk, etag };
      })();
      packagers.set(key, p);
      p.catch(() => packagers.delete(key)); // do not cache failures
    }
    return packagers.get(key);
  }

  return async function handle(request, env) {
    try {
      if (request.method !== "GET" && request.method !== "HEAD") {
        throw new HttpError(405, "method not allowed");
      }
      const path = decodeURIComponent(new URL(request.url).pathname).replace(/^\/+/, "");
      const cut = path.lastIndexOf("/");
      if (cut < 1) throw new HttpError(404, "expected /<object-key>/<resource>");
      const key = path.slice(0, cut);
      const name = path.slice(cut + 1);
      if (key.split("/").includes("..")) throw new HttpError(400, "bad key");

      const { pk, etag } = await packagerFor(key, env);
      let body;
      let type;
      let immutable = false;
      let m;
      if (name === "master.m3u8") {
        body = pk.hlsMaster();
        type = "application/vnd.apple.mpegurl";
      } else if (name === "manifest.mpd") {
        body = pk.dashMpd();
        type = "application/dash+xml";
      } else if ((m = /^track-(\d+)\.m3u8$/.exec(name))) {
        body = pk.hlsMedia(Number(m[1]));
        type = "application/vnd.apple.mpegurl";
      } else if ((m = /^init-(\d+)\.mp4$/.exec(name))) {
        body = pk.initSegment(Number(m[1]));
        type = "video/mp4";
        immutable = true;
      } else if ((m = /^seg-(\d+)-(\d+)\.m4s$/.exec(name))) {
        body = await pk.mediaSegment(Number(m[1]), Number(m[2]));
        type = "video/iso.segment";
        immutable = true;
      } else {
        throw new HttpError(404, `no such resource: ${name}`);
      }

      const headers = {
        "Content-Type": type,
        "Access-Control-Allow-Origin": "*",
        // Segments are a pure function of (object, name): safe to cache forever
        // at the CDN as long as the source object does not change (the ETag is
        // part of the validator).
        "Cache-Control": immutable ? "public, max-age=31536000, immutable" : "public, max-age=300",
      };
      if (etag) headers.ETag = `W/"${etag.replace(/"/g, "")}-${name}"`;
      if (etag && request.headers.get("If-None-Match") === headers.ETag) {
        return new Response(null, { status: 304, headers });
      }
      return new Response(request.method === "HEAD" ? null : body, { headers });
    } catch (e) {
      const message = typeof e === "string" ? e : (e && e.message) || String(e);
      let status = e instanceof HttpError ? e.status : 500;
      // The packager reports a bad track or segment number as "not found".
      if (status === 500 && /not found/.test(message)) status = 404;
      return new Response(message, {
        status,
        headers: { "Content-Type": "text/plain", "Access-Control-Allow-Origin": "*" },
      });
    }
  };
}
