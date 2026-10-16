import assert from "node:assert/strict";
import { join } from "node:path";
import { expect } from "@playwright/test";

export async function testMobileComposer(browser, base, screenshots, entry) {
  const context = await browser.newContext({ extraHTTPHeaders: { "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": "ag_browser_ui", "x-treer-interface-instance": "browser-ui" } });
  const page = await context.newPage();
  try {
    for (const width of [390, 767, 768, 1440]) {
      for (const look of width === 390 ? ["a", "b", "c"] : ["a"]) {
        for (const theme of width === 390 ? ["light", "dark"] : ["light"]) {
          await page.setViewportSize({ width, height: 900 });
          await page.goto(`${base}/${entry}?look=${look}&theme=${theme}&lang=en`);
          await expect(page.locator("#ta")).toBeEnabled();
          for (const focused of [false, true]) {
            if (focused) await page.locator("#ta").fill("A mobile prompt that stays above the action buttons.");
            const layout = await page.evaluate(() => {
              const area = document.querySelector("#ta").getBoundingClientRect();
              const controls = document.querySelector(".ctl").getBoundingClientRect();
              const box = document.querySelector("#box").getBoundingClientRect();
              return { separate: area.bottom <= controls.top, full: area.width > box.width - 50,
                hints: getComputedStyle(document.querySelector("#hint")).display,
                keyGlyph: getComputedStyle(document.querySelector(".send-t"), "::after").content,
                targets: [...document.querySelectorAll(".ctl button")].every(button => { const r = button.getBoundingClientRect(); return !r.width || (r.width >= 44 && r.height >= 44); }),
                overflow: document.documentElement.scrollWidth > innerWidth };
            });
            assert.equal(layout.overflow, false, `${width}/${look}/${theme}: no overflow`);
            if (width < 768) {
              assert.ok(layout.separate && layout.full && layout.targets, `${width}/${look}/${theme}/${focused}: input row and 44px controls ${JSON.stringify(layout)}`);
              assert.equal(layout.hints, "none");
              assert.equal(layout.keyGlyph, "none");
            } else if (!focused) assert.equal(layout.separate, false, "desktop compact layout unchanged");
            else assert.notEqual(layout.hints, "none", "desktop shortcuts retained");
          }
          if (width === 390) await page.screenshot({ path: join(screenshots, `mobile-composer-${look}-${theme}.png`), animations: "disabled" });
        }
      }
    }
  } finally { await context.close(); }
}
