import { createHash } from "node:crypto";
import { IMAGE_FILE_LIMIT } from "../image-format.js";

/** Native history may change message IDs during replay. Reconcile only with
 * this Agent/session's retained registry; never forward bytes/digests to UI.
 */
export function imageDigest(content: unknown): string | undefined {
  if (!content || typeof content !== "object") return;
  const block = content as Record<string, unknown>;
  if (block.type !== "image") return;
  const source = block.source && typeof block.source === "object" ? block.source as Record<string, unknown> : block;
  const data = source.data;
  if (typeof data !== "string" || data.length > Math.ceil(IMAGE_FILE_LIMIT / 3) * 4 || !/^[A-Za-z0-9+/]*={0,2}$/.test(data)) return;
  return createHash("sha256").update(Buffer.from(data, "base64")).digest("hex");
}
