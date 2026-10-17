import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { createServer, request as httpRequest } from "node:http";
import { expect } from "@playwright/test";


// A stand-in for the Proxy: it forwards every request (downloads included) to the launcher with the identity markers the real
// Proxy adds. Playwright's extraHTTPHeaders do not reach download navigations, so the browser here talks to this instead.
async function startStandInProxy(upstream) {
  const target = new URL(upstream);
  const server = createServer((request, response) => {
    const forwarded = httpRequest({ hostname: target.hostname, port: target.port, path: request.url, method: request.method, headers: {
      ...request.headers, host: target.host,
      "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": "ag_browser_ui", "x-treer-interface-instance": "browser-ui",
    } }, (upstreamResponse) => { response.writeHead(upstreamResponse.statusCode, upstreamResponse.headers); upstreamResponse.pipe(response); });
    forwarded.on("error", () => response.destroy());
    request.pipe(forwarded);
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return { url: `http://127.0.0.1:${server.address().port}`, close: () => new Promise((resolve) => server.close(resolve)) };
}

// File downloads from the Files panel: a link per file row (hover/focus on desktop, always on touch at 44px), the preview
// header button, keyboard access, and streamed bytes with the server's attachment filename.
export async function testFilesDownloadUi(browser, upstream, screenshots, entry = "index.html") {
  const proxy = await startStandInProxy(upstream);
  const base = proxy.url;
  for (const width of [1440, 390]) {
    const context = await browser.newContext({ viewport: { width, height: 900 }, acceptDownloads: true, hasTouch: width < 700, isMobile: width < 700 });
    const page = await context.newPage(), errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    try {
      await page.goto(`${base}/${entry}?lang=en`);
      if (width >= 700) await page.getByRole("button", { name: "Browse files" }).click();
      else { await page.getByRole("button", { name: "Menu", exact: true }).click(); await page.getByRole("menuitem", { name: "Files", exact: true }).click(); }
      const panel = page.locator("#files");
      await expect(panel).toBeVisible();
      const tree = panel.getByRole("tree", { name: "Files" });
      await expect(tree.getByRole("treeitem", { name: "README.md" })).toBeVisible();
      const link = (name) => panel.getByRole("link", { name: `Download ${name}`, exact: true });
      // Folders have no download link (no folder/zip download in v1).
      await expect(panel.getByRole("link", { name: /^Download (src|empty)$/ })).toHaveCount(0);
      const opacity = (locator) => locator.evaluate((element) => Number(getComputedStyle(element).opacity));
      if (width >= 700) {
        await expect.poll(() => opacity(link("README.md"))).toBe(0);
        await tree.getByRole("treeitem", { name: "README.md" }).hover();
        await expect.poll(() => opacity(link("README.md"))).toBe(1);
      } else {
        await expect.poll(() => opacity(link("README.md"))).toBe(1);
        const box = await link("README.md").boundingBox();
        assert.ok(box.width >= 43.5 && box.height >= 43.5, `touch target ${box.width}x${box.height}`);
      }
      // Row link: streams the exact bytes with the attachment filename.
      const [fromRow] = await Promise.all([page.waitForEvent("download"), link("README.md").click()]);
      assert.equal(fromRow.suggestedFilename(), "README.md");
      assert.equal(await readFile(await fromRow.path(), "utf8"), "# Workspace\nSmall tools, thoughtful interfaces.\n");
      // A name with # and % survives the URL and the header.
      const [special] = await Promise.all([page.waitForEvent("download"), link("special # 100%.txt").click()]);
      assert.equal(special.suggestedFilename(), "special # 100%.txt");
      assert.equal(await readFile(await special.path(), "utf8"), "literal percent and hash filename\n");
      // Preview header button downloads the selected file, in the preview tab on phones.
      await tree.getByRole("treeitem", { name: "large.txt" }).click();
      const header = panel.locator(".pmeta").getByRole("link", { name: /^Download/ });
      await expect(header).toBeVisible();
      if (width < 700) { const box = await header.boundingBox(); assert.ok(box.height >= 43.5, `header target ${box.height}`); }
      const [fromHeader] = await Promise.all([page.waitForEvent("download"), header.click()]);
      assert.equal(fromHeader.suggestedFilename(), "large.txt");
      assert.equal((await readFile(await fromHeader.path(), "utf8")).length, 65537);
      await page.screenshot({ path: join(screenshots, `files-download-${width}.png`) });
      if (width >= 700) {
        // Keyboard: ArrowRight on a file row moves to its download link, Enter downloads, ArrowLeft returns to the row.
        await tree.getByRole("treeitem", { name: "README.md" }).focus();
        await page.keyboard.press("ArrowRight");
        await expect(link("README.md")).toBeFocused();
        const [fromKey] = await Promise.all([page.waitForEvent("download"), page.keyboard.press("Enter")]);
        assert.equal(fromKey.suggestedFilename(), "README.md");
        await page.keyboard.press("ArrowLeft");
        await expect(tree.getByRole("treeitem", { name: "README.md" })).toBeFocused();
        // The links are not extra tab stops of the tree (one roving stop); the preview button is reachable by Tab.
        assert.equal(await panel.locator("[data-dl]").evaluateAll((nodes) => nodes.every((node) => node.tabIndex === -1)), true);
      }
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
      assert.deepEqual(errors, []);
    } finally { await context.close(); }
  }
  await proxy.close();
}
