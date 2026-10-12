const system = window.matchMedia("(prefers-color-scheme: dark)");
const valid = (theme) => theme === "light" || theme === "dark";
let chosen = new URLSearchParams(window.location.search).get("theme");
function apply() {
  document.documentElement.dataset.theme = valid(chosen) ? chosen : system.matches ? "dark" : "light";
}
system.addEventListener("change", () => { if (!valid(chosen)) apply(); });
window.addEventListener("message", (event) => {
  if (event.source !== window.parent || event.data?.type !== "treer:embed-theme" || !valid(event.data.theme)) return;
  chosen = event.data.theme;
  apply();
});
apply();
