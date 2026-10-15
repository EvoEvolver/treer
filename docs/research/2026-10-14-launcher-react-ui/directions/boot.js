// Synchronous, in <head>: picks the look and theme before first paint. Sources:
//   look  : ?look=a|b|c  >  localStorage "treer-launcher-look"  >  default (b)   [whitelisted; storage wrapped in try/catch]
//   theme : parent postMessage treer:embed-theme / ?theme=  >  stored manual mode  >  system preference
// Look and theme are orthogonal: 3 looks x light/dark = 6 variants.
(() => {
  "use strict";
  const root = document.documentElement, LOOKS = ["a", "b", "c"], LOOK_KEY = "treer-launcher-look", THEME_KEY = "treer-launcher-theme", DEFAULT_LOOK = "b";
  const read = (k) => { try { return localStorage.getItem(k); } catch { return null; } };
  const write = (k, v) => { try { localStorage.setItem(k, v); } catch { /* private mode or blocked storage */ } };
  const okLook = (v) => (LOOKS.includes(v) ? v : null), okTheme = (v) => (v === "light" || v === "dark" ? v : null);
  const params = new URLSearchParams(location.search), locked = root.hasAttribute("data-look-lock"), media = matchMedia("(prefers-color-scheme: dark)");
  let look = locked ? root.dataset.look : okLook(params.get("look")) || okLook(read(LOOK_KEY)) || DEFAULT_LOOK;
  let override = okTheme(params.get("theme"));          // explicit: URL or parent window, not stored
  let mode = override || (["light", "dark", "system"].includes(read(THEME_KEY)) ? read(THEME_KEY) : "system");
  const listeners = [];
  function apply() {
    root.dataset.look = look;
    const effective = override || (okTheme(mode) ?? (media.matches ? "dark" : "light"));
    root.dataset.theme = effective;
    listeners.forEach((f) => f({ look, theme: effective, mode: override || mode, locked }));
  }
  media.addEventListener("change", () => { if (!override && mode === "system") apply(); });
  addEventListener("message", (e) => { if (e.source === parent && e.data?.type === "treer:embed-theme" && okTheme(e.data.theme)) { override = e.data.theme; apply(); } });
  window.TreerAppearance = {
    LOOKS,
    setLook(v) { if (locked || !okLook(v)) return; look = v; write(LOOK_KEY, v); apply(); },
    setTheme(m) { if (!["system", "light", "dark"].includes(m)) return; override = null; mode = m; write(THEME_KEY, m); apply(); },
    toggleTheme() { this.setTheme(root.dataset.theme === "dark" ? "light" : "dark"); },
    subscribe(f) { listeners.push(f); f({ look, theme: root.dataset.theme, mode: override || mode, locked }); },
  };
  apply();
})();
