// Cloudflare Workers entry point. Build the WASM package first:
//   wasm-pack build tpt-kinetix-package --target web --release \
//       --out-dir ../examples/edge-worker/pkg -- --features wasm
import wasmModule from "./pkg/tpt_kinetix_package_bg.wasm";
import { WasmPackager, initSync } from "./pkg/tpt_kinetix_package.js";
import { createHandler } from "./handler.mjs";

const handle = createHandler({ WasmPackager, initSync, wasmModule });

export default {
  fetch(request, env) {
    return handle(request, env);
  },
};
