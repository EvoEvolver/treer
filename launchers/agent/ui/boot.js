// Runs synchronously from <head>, before first paint. It decides the theme and exposes it to the UI.
//   theme: parent postMessage {type:"treer:embed-theme"} or ?theme=  >  stored mode "treer-launcher-theme"  >  system preference
//   lang : ?lang=en|zh at load (the message carries it but a language change needs a reload).
// The Console may also send a `look`; this page has a single design and ignores it. Every input is validated.
(() => {
  "use strict";
  const root = document.documentElement;
  const THEME_KEY = "treer-launcher-theme";
  const read = (key) => { try { return localStorage.getItem(key); } catch { return null; } };
  const write = (key, value) => { try { localStorage.setItem(key, value); } catch { /* storage unavailable */ } };
  const okTheme = (value) => (value === "light" || value === "dark" ? value : null);
  const okMode = (value) => (value === "light" || value === "dark" || value === "system" ? value : null);
  const params = new URLSearchParams(location.search);
  const media = matchMedia("(prefers-color-scheme: dark)");
  let override = okTheme(params.get("theme"));
  let mode = okMode(read(THEME_KEY)) || "system";
  const listeners = new Set();
  const snapshot = () => ({ theme: root.dataset.theme, mode: override || mode });
  function apply() {
    root.dataset.theme = override || okTheme(mode) || (media.matches ? "dark" : "light");
    const state = snapshot();
    for (const listener of listeners) listener(state);
  }
  media.addEventListener("change", () => { if (!override && mode === "system") apply(); });
  // Standalone pages never inherit appearance from another window. In a frame,
  // the Console supplies its own origin explicitly (appearance only, not auth).
  let parentOrigin = null;
  try {
    const configured = params.has("embed_origin") ? params.get("embed_origin") : location.origin;
    const candidate = new URL(configured);
    if (["http:", "https:"].includes(candidate.protocol) && !candidate.hostname.includes("*") && candidate.origin === configured) parentOrigin = configured;
  } catch { /* malformed configuration disables parent messages */ }
  if (window.top !== window && parentOrigin) addEventListener("message", (event) => {
    if (event.source !== parent || event.origin !== parentOrigin || event.data?.type !== "treer:embed-theme" || !okTheme(event.data.theme)) return;
    override = event.data.theme;
    apply();
  });
  window.TreerAppearance = {
    get: snapshot,
    setTheme(value) { if (!okMode(value)) return; override = null; mode = value; write(THEME_KEY, value); apply(); },
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
  };
  apply();
})();
