// Runs synchronously from <head> so the first paint already uses the right theme.
// Stored choice > system preference. The value is validated against a fixed list.
(() => {
  "use strict";
  let stored = null;
  try { stored = localStorage.getItem("issues-theme"); } catch { /* storage unavailable */ }
  const dark = matchMedia("(prefers-color-scheme: dark)").matches;
  document.documentElement.dataset.theme = stored === "light" || stored === "dark" ? stored : dark ? "dark" : "light";
})();
