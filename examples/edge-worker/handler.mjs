// Just-in-time HLS/DASH packaging in a Worker (Cloudflare Workers, Deno Deploy,
// Fastly Compute, or Node 18+): the MP4 stays in object storage; only its index
// and the requested segment's byte ranges are fetched.
//
//   GET /<object-key>/master.m3u8
//   GET /<object-key>/track-0.m3u8   init-0.mp4   seg-0-3.m4s   manifest.mpd
//
// Origin is either an R2-style binding (`env.BUCKET`, with `get(key, {range})`
// and `head(key)`) or plain HTTP range requests against `env.ORIGIN_BASE/<key>`.
//
// Edge cache: init and segment responses are immutable for a given object key,
// so they are stored in `caches.default` (the Cache API, where it exists) and
// answered from there without touching the origin or the packager. The cache key
// is the URL, so a replaced source object must get a new object key; the ETag
// only guards the browser's own revalidation. Binary responses honour a single
// `Range` header (206 / 416), so a player can fetch part of a segment.

const SEGMENT_SECONDS = 6;
const IMMUTABLE = /^(init-\d+\.mp4|seg-\d+-\d+\.m4s|iframes-\d+\.m4s|sub-\d+-\d+\.vtt)$/;

class HttpError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

/**
 * The byte range a `Range` header asks for: `{start, end}` (inclusive), `"unsatisfiable"`,
 * or `null` when the header is absent or not a single range this handler serves (then
 * the whole body is sent, which RFC 9110 allows).
 */
export function parseRange(header, length) {
  const m = /^bytes=(\d*)-(\d*)$/.exec(header || "");
  if (!m || (m[1] === "" && m[2] === "")) return null;
  let start;
  let end;
  if (m[1] === "") {
    // Suffix range: the last N bytes.
    start = Math.max(0, length - Number(m[2]));
    end = length - 1;
  } else {
    start = Number(m[1]);
    end = m[2] === "" ? length - 1 : Math.min(Number(m[2]), length - 1);
  }
  if (length === 0 || start > end || start >= length) return "unsatisfiable";
  return { start, end };
}

/** A binary response for `bytes`, honouring one `Range` header. */
function binaryResponse(request, bytes, headers) {
  const h = { ...headers, "Accept-Ranges": "bytes" };
  const head = request.method === "HEAD";
  const range = parseRange(request.headers.get("Range"), bytes.length);
  if (range === "unsatisfiable") {
    return new Response(null, { status: 416, headers: { ...h, "Content-Range": `bytes */${bytes.length}` } });
  }
  if (range) {
    return new Response(head ? null : bytes.subarray(range.start, range.end + 1), {
      status: 206,
      headers: { ...h, "Content-Range": `bytes ${range.start}-${range.end}/${bytes.length}` },
    });
  }
  return new Response(head ? null : bytes, { status: 200, headers: h });
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

  /** The 304 for a validator match, or `null`. */
  function notModified(request, headers) {
    if (headers.ETag && request.headers.get("If-None-Match") === headers.ETag) {
      return new Response(null, { status: 304, headers });
    }
    return null;
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

      // Immutable resources: answer from the edge cache before any origin read.
      const immutable = IMMUTABLE.test(name);
      const cache = immutable && request.method === "GET" ? globalThis.caches?.default : undefined;
      const cacheKey = cache ? new Request(request.url, { method: "GET" }) : null;
      if (cache) {
        const hit = await cache.match(cacheKey);
        if (hit) {
          const headers = Object.fromEntries(hit.headers.entries());
          return notModified(request, headers) ?? binaryResponse(request, new Uint8Array(await hit.arrayBuffer()), headers);
        }
      }

      const { pk, etag } = await packagerFor(key, env);
      let body;
      let type;
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
      } else if ((m = /^seg-(\d+)-(\d+)\.m4s$/.exec(name))) {
        body = await pk.mediaSegment(Number(m[1]), Number(m[2]));
        type = "video/iso.segment";
      } else if ((m = /^iframes-(\d+)\.m3u8$/.exec(name))) {
        // Trick-play: the I-frames-only playlist of video track N.
        body = await pk.iframePlaylist(Number(m[1]));
        type = "application/vnd.apple.mpegurl";
      } else if ((m = /^iframes-(\d+)\.m4s$/.exec(name))) {
        body = await pk.iframeFile(Number(m[1]));
        type = "video/iso.segment";
      } else if ((m = /^subs-(\d+)\.m3u8$/.exec(name))) {
        // WebVTT subtitle track N: its media playlist (absent when there is no such track).
        body = pk.subtitleMediaPlaylist(Number(m[1]));
        if (body == null) throw new HttpError(404, `no such subtitle track: ${m[1]}`);
        type = "application/vnd.apple.mpegurl";
      } else if ((m = /^sub-(\d+)-(\d+)\.vtt$/.exec(name))) {
        body = await pk.subtitleSegment(Number(m[1]), Number(m[2]));
        type = "text/vtt";
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
      const nm = notModified(request, headers);
      if (nm) return nm;

      if (!immutable) {
        return new Response(request.method === "HEAD" ? null : body, { headers });
      }
      const bytes = body instanceof Uint8Array ? body : new Uint8Array(body);
      if (cache) {
        // Store the whole 200 response, never a 206, and never fail the request
        // because the cache refused it.
        try {
          await cache.put(cacheKey, new Response(bytes, { status: 200, headers }));
        } catch {
          // Cache write failures are not request failures.
        }
      }
      return binaryResponse(request, bytes, headers);
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
