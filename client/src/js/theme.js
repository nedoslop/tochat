// Theme management. Three modes: "system", "light", "dark". The mode is
// persisted to `<data_dir>/theme.json` via Tauri. Visual application is
// done by setting/removing the `data-theme` attribute on <html>:
//
//   - "system": no attribute → CSS @media (prefers-color-scheme) decides
//   - "light" : data-theme="light"
//   - "dark"  : data-theme="dark"

import { invoke } from "./api.js";

const ORDER = ["system", "light", "dark"];
const ICONS = { system: "🌓", light: "☀️", dark: "🌙" };

let current = "system";
let systemListenerAttached = false;

/** Applies a theme mode to the DOM and updates every theme button icon. */
export function applyTheme(mode) {
  const root = document.documentElement;
  if (mode === "system") {
    root.removeAttribute("data-theme");
  } else {
    root.setAttribute("data-theme", mode);
  }

  const icon = ICONS[mode] ?? ICONS.system;
  const label = `Theme: ${mode} (click to change)`;
  for (const id of ["theme-btn", "theme-btn-login"]) {
    const el = document.getElementById(id);
    if (!el) continue;
    el.textContent = icon;
    el.title = label;
  }
}

/**
 * Loads the persisted theme and applies it. Safe to call before login —
 * the theme is a global preference, not per-user.
 */
export async function initTheme() {
  try {
    const mode = await invoke("get_theme");
    if (ORDER.includes(mode)) current = mode;
  } catch (_) {
    current = "system";
  }
  applyTheme(current);
  attachSystemListener();
}

/** Cycles through system → light → dark → system and persists the choice. */
export async function cycleTheme() {
  const next = ORDER[(ORDER.indexOf(current) + 1) % ORDER.length];
  current = next;
  applyTheme(next);
  try {
    await invoke("set_theme", { theme: next });
  } catch (e) {
    console.error("save theme failed:", e);
  }
}

/**
 * When the user is on "system", we re-apply the theme whenever the OS
 * preference flips. (No visible attribute change is needed in that case —
 * the CSS media query handles it — but this keeps button icons honest and
 * gives us one place to hook in if we ever add a live preview.)
 */
function attachSystemListener() {
  if (systemListenerAttached) return;
  systemListenerAttached = true;
  const mq = window.matchMedia("(prefers-color-scheme: dark)");
  const handler = () => {
    if (current === "system") applyTheme("system");
  };
  if (mq.addEventListener) mq.addEventListener("change", handler);
  else if (mq.addListener) mq.addListener(handler);
}
