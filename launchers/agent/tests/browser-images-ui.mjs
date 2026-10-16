import assert from "node:assert/strict";
import { join } from "node:path";
import { expect } from "@playwright/test";

export async function testImagesUi(browser, base, screenshots, entry = "index.html") {
  for (const width of [1440, 390]) {
    const context = await browser.newContext({ viewport: { width, height: 900 }, extraHTTPHeaders: { "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": "ag_browser_ui", "x-treer-interface-instance": "browser-ui" } });
    const page = await context.newPage(), errors = [];
    page.on("pageerror", error => errors.push(error.message));
    try {
      await page.goto(`${base}/${entry}?lang=en`);
      await expect(page.getByRole("button", { name: "Attach images" })).toBeEnabled();
      // Real ClipboardEvent/DataTransfer File input follows the same handler as
      // Cmd/Ctrl+V and mobile paste; no clipboard permissions/provider needed.
      await page.evaluate(async () => {
        const canvas = document.createElement("canvas"); canvas.width = 2; canvas.height = 2;
        canvas.getContext("2d").fillRect(0, 0, 2, 2);
        const blob = await new Promise(resolve => canvas.toBlob(resolve, "image/png"));
        window.imageBytes = [...new Uint8Array(await blob.arrayBuffer())];
        const transfer = new DataTransfer(); transfer.items.add(new File([blob], '../<img onerror="window.imageXSS=1">.png', { type: "image/png" }));
        document.querySelector("#ta").dispatchEvent(new ClipboardEvent("paste", { clipboardData: transfer, bubbles: true, cancelable: true }));
      });
      await expect(page.locator(".image-drafts img")).toHaveCount(1);
      await page.evaluate(() => {
        const transfer = new DataTransfer(); transfer.items.add(new File([new Uint8Array(window.imageBytes)], "drop.png", { type: "image/png" }));
        document.querySelector("#composer").dispatchEvent(new DragEvent("dragover", { dataTransfer: transfer, bubbles: true, cancelable: true }));
      });
      await expect(page.locator("#composer")).toHaveClass(/image-drop/);
      await page.evaluate(() => {
        const transfer = new DataTransfer(); transfer.items.add(new File([new Uint8Array(window.imageBytes)], "drop.png", { type: "image/png" }));
        document.querySelector("#composer").dispatchEvent(new DragEvent("drop", { dataTransfer: transfer, bubbles: true, cancelable: true }));
      });
      await expect(page.locator("#composer")).not.toHaveClass(/image-drop/);
      await expect(page.locator(".image-drafts img")).toHaveCount(2);
      await expect.poll(() => page.evaluate(() => {
        const last = document.querySelector(".chat > :last-child");
        return !last || last.getBoundingClientRect().bottom <= document.querySelector("#dock").getBoundingClientRect().top;
      })).toBe(true);
      await expect(page.locator(".image-drafts")).toContainText("including metadata");
      for (const image of await page.locator(".image-drafts img").all()) {
        assert.match(await image.getAttribute("src"), /^api\/attachments\/img_[a-f0-9]{32}$/);
        assert.equal(await image.evaluate(node => node.complete && node.naturalWidth > 0), true);
      }
      await page.screenshot({ path: join(screenshots, `images-draft-${width}.png`) });
      // Removal is a real guarded DELETE. Keyboard focus reaches the action.
      const remove = page.getByRole("button", { name: "Remove image 2" });
      await remove.focus(); await page.keyboard.press("Enter");
      await expect(page.locator(".image-drafts img")).toHaveCount(1);
      const prompt = width === 390 ? "" : `Describe the uploaded image ${width}`;
      await page.locator("#ta").fill(prompt);
      await page.locator("#send").click();
      await expect(page.locator(".image-drafts img")).toHaveCount(0);
      const message = page.locator(".msg.user").filter({ has: page.locator(".sent-images") }).last();
      await expect(message.locator("img")).toHaveCount(1);
      await page.reload();
      await expect(message.locator("img")).toHaveCount(1);
      await expect.poll(() => message.locator("img").evaluate(node => node.complete && node.naturalWidth > 0)).toBe(true);
      await expect(page.locator("#thread")).toHaveAttribute("data-view", "chat");
      await page.evaluate(() => { const thread = document.querySelector("#thread"); thread.scrollTop = thread.scrollHeight; });
      assert.equal(await page.evaluate(() => window.imageXSS), undefined);
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
      await page.screenshot({ path: join(screenshots, `images-sent-${width}.png`), animations: "disabled" });
      await page.goto(`${base}/${entry}?lang=en&theme=dark`);
      await expect(page.locator("#thread")).toHaveAttribute("data-view", "chat");
      await expect.poll(() => message.locator("img").evaluate(node => node.complete && node.naturalWidth > 0)).toBe(true);
      await page.evaluate(() => { const thread = document.querySelector("#thread"); thread.scrollTop = thread.scrollHeight; });
      await page.screenshot({ path: join(screenshots, `images-sent-dark-${width}.png`), animations: "disabled" });
      // Oversize files are rejected in the browser before upload; a spoofed MIME
      // is independently rejected by the server with a fixed error.
      await page.locator('input[type="file"]').setInputFiles({ name: "large.png", mimeType: "image/png", buffer: Buffer.alloc(10 * 1024 * 1024 + 1) });
      await expect(page.getByRole("alert")).toContainText("10 MiB");
      const spoof = await page.evaluate(async () => {
        const response = await fetch("api/attachments", { method: "POST", body: "MZexecutable", headers: { "content-type": "image/png" } });
        return { status: response.status, body: await response.json() };
      });
      assert.equal(spoof.status, 400); assert.equal(spoof.body.error, "attachment_invalid_image");
      if (width === 1440) {
        for (const mime of ["image/jpeg", "image/webp"]) {
          const result = await page.evaluate(async mime => {
            const canvas = document.createElement("canvas"); canvas.width = 2; canvas.height = 2;
            const blob = await new Promise(resolve => canvas.toBlob(resolve, mime));
            const uploaded = await fetch("api/attachments", { method: "POST", body: blob });
            const body = await uploaded.json();
            const truncated = (await blob.arrayBuffer()).slice(0, blob.size - 1);
            const rejected = await fetch("api/attachments", { method: "POST", body: truncated });
            return { status: uploaded.status, mime: body.attachment?.mime, truncatedStatus: rejected.status };
          }, mime);
          assert.deepEqual(result, { status: 201, mime, truncatedStatus: 400 });
        }
      }
      assert.deepEqual(errors, []);
    } finally { await context.close(); }
  }
}
