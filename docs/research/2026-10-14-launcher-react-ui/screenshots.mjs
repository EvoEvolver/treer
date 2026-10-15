// Regenerates the PNGs next to this file. Run from the repo root after `npm ci` in launchers/agent:
//   node docs/research/2026-10-14-launcher-react-ui/screenshots.mjs
import { chromium } from "../../../launchers/agent/node_modules/playwright-core/index.mjs";
import { fileURLToPath } from "node:url";
const dir = fileURLToPath(new URL(".", import.meta.url));
const states = [
  // [name, query, themes, focus Send with the keyboard]
  ["default", "", ["light", "dark"]],
  ["plan", "?pop=plan", ["light", "dark"]],
  ["queue", "?pop=queue", ["light"]],
  ["goal", "?pop=goal", ["light"]],
  ["expanded", "?composer=expanded", ["light"]],
  ["scrolled", "?scroll=up", ["light"]],
  ["zh", "?lang=zh", ["light"]],
  ["zh-plan", "?lang=zh&pop=plan", ["light"]],
  ["focus", "", ["light"], true],
  ["files", "?files=1", ["light"]],
];
const browser = await chromium.launch();
for (const [vp, width, height] of [["desktop", 1280, 800], ["390", 390, 844]]) {
  for (const [name, query, themes, focus] of states) {
    if (vp === "desktop" && name === "files") continue; // the drawer is always open on desktop
    for (const theme of themes) {
      const page = await browser.newPage({ viewport: { width, height }, deviceScaleFactor: 2 });
      await page.goto(`file://${dir}prototype.html${query}`);
      await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, theme);
      if (focus) { await page.keyboard.press("Shift"); await page.focus("#send"); }
      await page.screenshot({ path: `${dir}${vp}-${name}-${theme}.png` });
      if (vp === "390" && await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)) console.log("OVERFLOW", name, theme);
      await page.close();
    }
  }
}
await browser.close();
