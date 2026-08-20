import { defineConfig, type Plugin, type ViteDevServer } from "vite";
import { execSync } from "node:child_process";
import path from "node:path";

const root = path.dirname(new URL(import.meta.url).pathname);

/** Build the wasm crate with wasm-pack; rebuild + reload when Rust changes. */
function wasmPack(): Plugin {
  let building = false;
  const build = () => {
    building = true;
    try {
      execSync("wasm-pack build wasm --target web --dev --out-dir pkg", {
        cwd: root,
        stdio: "inherit",
      });
    } finally {
      building = false;
    }
  };
  return {
    name: "wasm-pack",
    buildStart() {
      build();
    },
    configureServer(server: ViteDevServer) {
      // Watch the sandbox wasm crate AND the kurbo-se crate itself.
      server.watcher.add([
        path.resolve(root, "wasm/src"),
        path.resolve(root, "../src"),
      ]);
      server.watcher.on("change", (file: string) => {
        if (file.endsWith(".rs") && !building) {
          try {
            build();
            server.ws.send({ type: "full-reload" });
          } catch (e) {
            console.error("wasm-pack build failed:", e);
          }
        }
      });
    },
  };
}

export default defineConfig({
  plugins: [wasmPack()],
  build: { target: "es2022" }, // top-level await in main.ts
  server: { fs: { allow: [root] } },
});
