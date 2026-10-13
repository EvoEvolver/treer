import type { IncomingMessage } from "node:http";

export class HttpInputError extends Error {
  constructor(message: string, readonly status: number) { super(message); }
}
export const JSON_BODY_LIMIT = 128 * 1024;

/** Enforce a bound while streaming, including bodies without Content-Length. */
export async function readJson(request: IncomingMessage): Promise<Record<string, unknown>> {
  if (Number(request.headers["content-length"]) > JSON_BODY_LIMIT) throw new HttpInputError("JSON body exceeds 128 KiB", 413);
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of request) {
    const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    size += bytes.length;
    if (size > JSON_BODY_LIMIT) throw new HttpInputError("JSON body exceeds 128 KiB", 413);
    chunks.push(bytes);
  }
  if (!chunks.length) return {};
  let value: unknown;
  try { value = JSON.parse(Buffer.concat(chunks).toString("utf8")); }
  catch { throw new HttpInputError("Invalid JSON body", 400); }
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new HttpInputError("JSON body must be an object", 400);
  return value as Record<string, unknown>;
}
