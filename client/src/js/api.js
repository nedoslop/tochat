// Thin wrapper around the Tauri globals so the rest of the code can just
// `import { invoke, listen } from "./api.js"`.

const tauri = window.__TAURI__ || {};

if (!tauri.core) {
  console.error("[api] Tauri core is not available");
}

export const invoke =
  tauri.core?.invoke ??
  (async () => {
    throw new Error("Tauri is not available");
  });

export const listen =
  tauri.event?.listen ??
  (async () => {
    throw new Error("Tauri is not available");
  });
