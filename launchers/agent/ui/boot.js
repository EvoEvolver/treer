// Runs synchronously from <head>, before first paint. It decides the look and the theme and exposes them to the UI.
//   look : ?look=a|b|c  >  localStorage "treer-launcher-look"  >  default (b)
//   look  also from the parent message (session only). lang: ?lang=en|zh at load (the message carries it but a language change needs a reload)
//   theme: parent postMessage {type:"treer:embed-theme"} or ?theme=  >  stored mode "treer-launcher-theme"  >  system preference
// Look and theme are independent (3 looks x light/dark). Every input is validated against a fixed list.
(() => {
  "use strict";
  const root = document.documentElement;
  const LOOKS = ["a", "b", "c"], LOOK_KEY = "treer-launcher-look", THEME_KEY = "treer-launcher-theme", DEFAULT_LOOK = "b";
  const read = (key) => { try { return localStorage.getItem(key); } catch { return null; } };
  const write = (key, value) => { try { localStorage.setItem(key, value); } catch { /* storage unavailable */ } };
  const okLook = (value) => (LOOKS.includes(value) ? value : null);
  const okTheme = (value) => (value === "light" || value === "dark" ? value : null);
  const okMode = (value) => (value === "light" || value === "dark" || value === "system" ? value : null);
  const params = new URLSearchParams(location.search);
  const media = matchMedia("(prefers-color-scheme: dark)");
  let look = okLook(params.get("look")) || okLook(read(LOOK_KEY)) || DEFAULT_LOOK;
  let override = okTheme(params.get("theme"));
  let mode = okMode(read(THEME_KEY)) || "system";
  const listeners = new Set();
  const snapshot = () => ({ look, theme: root.dataset.theme, mode: override || mode });
  function apply() {
    root.dataset.look = look;
    root.dataset.theme = override || okTheme(mode) || (media.matches ? "dark" : "light");
    const state = snapshot();
    for (const listener of listeners) listener(state);
  }
  media.addEventListener("change", () => { if (!override && mode === "system") apply(); });
  addEventListener("message", (event) => {
    if (event.source !== parent || event.data?.type !== "treer:embed-theme" || !okTheme(event.data.theme)) return;
    override = event.data.theme;
    const sent = okLook(event.data.look);
    if (sent) look = sent; // session-only: the console's look is not written to the launcher's own storage
    apply();
  });
  window.TreerAppearance = {
    looks: LOOKS,
    get: snapshot,
    setLook(value) { if (!okLook(value)) return; look = value; write(LOOK_KEY, value); apply(); },
    setTheme(value) { if (!okMode(value)) return; override = null; mode = value; write(THEME_KEY, value); apply(); },
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
  };
  apply();
})();
