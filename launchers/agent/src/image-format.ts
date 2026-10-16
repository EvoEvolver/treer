import type { ImageAttachment } from "./model.js";

export class AttachmentError extends Error {
  constructor(readonly code: string, readonly status = 400) { super(code); }
}
export const IMAGE_FILE_LIMIT = 10 * 1024 * 1024;
export const IMAGE_PROMPT_LIMIT = 24 * 1024 * 1024;
export const IMAGE_COUNT_LIMIT = 8;
export const ATTACHMENT_ID = /^img_[a-f0-9]{32}$/;

function invalid(): never { throw new AttachmentError("attachment_invalid_image"); }
function dimensions(width: number, height: number) {
  if (!width || !height || width > 16384 || height > 16384 || width * height > 100_000_000) invalid();
}
function crc32(data: Buffer) {
  let crc = 0xffffffff;
  for (const byte of data) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
  }
  return (crc ^ 0xffffffff) >>> 0;
}

/** Validate container structure as well as magic; reject truncation and trailing payloads.
 * This is not an image decoder or a claim that arbitrary image metadata is harmless.
 */
export function imageFormat(bytes: Buffer, depth = 0): { mime: ImageAttachment["mime"]; extension: string } {
  if (depth > 1) invalid();
  if (!bytes.length) throw new AttachmentError("attachment_empty");
  if (bytes.length > IMAGE_FILE_LIMIT) throw new AttachmentError("attachment_too_large", 413);
  if (bytes.subarray(0, 8).equals(Buffer.from([137,80,78,71,13,10,26,10]))) {
    let offset = 8, header = false, pixels = false;
    while (offset + 12 <= bytes.length) {
      const size = bytes.readUInt32BE(offset), end = offset + 12 + size;
      if (end > bytes.length) invalid();
      const kind = bytes.toString("ascii", offset + 4, offset + 8);
      if (!/^[A-Za-z]{4}$/.test(kind) || crc32(bytes.subarray(offset + 4, end - 4)) !== bytes.readUInt32BE(end - 4)) invalid();
      if (!header && kind !== "IHDR") invalid();
      if (kind === "IHDR") {
        if (header || size !== 13) invalid();
        dimensions(bytes.readUInt32BE(offset + 8), bytes.readUInt32BE(offset + 12));
        header = true;
      }
      if (kind === "IDAT") { if (!size) invalid(); pixels = true; }
      if (kind === "IEND") {
        if (size || !pixels || end !== bytes.length) invalid();
        return { mime: "image/png", extension: "png" };
      }
      offset = end;
    }
    invalid();
  }
  if (["GIF87a", "GIF89a"].includes(bytes.toString("ascii", 0, 6))) {
    if (bytes.length < 14) invalid();
    dimensions(bytes.readUInt16LE(6), bytes.readUInt16LE(8));
    let offset = 13 + (bytes[10] & 128 ? 3 * (2 ** ((bytes[10] & 7) + 1)) : 0), images = 0;
    const blocks = () => {
      while (offset < bytes.length) {
        const length = bytes[offset++];
        if (!length) return;
        offset += length;
        if (offset > bytes.length) invalid();
      }
      invalid();
    };
    while (offset < bytes.length) {
      const type = bytes[offset++];
      if (type === 0x3b) {
        if (!images || offset !== bytes.length) invalid();
        return { mime: "image/gif", extension: "gif" };
      }
      if (type === 0x21) { if (offset >= bytes.length) invalid(); offset++; blocks(); }
      else if (type === 0x2c) {
        if (offset + 10 > bytes.length) invalid();
        dimensions(bytes.readUInt16LE(offset + 4), bytes.readUInt16LE(offset + 6));
        const flags = bytes[offset + 8];
        offset += 9 + (flags & 128 ? 3 * (2 ** ((flags & 7) + 1)) : 0);
        if (offset >= bytes.length || bytes[offset] < 2 || bytes[offset] > 8) invalid();
        offset++; blocks(); images++;
      } else invalid();
    }
    invalid();
  }
  if (bytes.toString("ascii", 0, 4) === "RIFF" && bytes.toString("ascii", 8, 12) === "WEBP") {
    if (bytes.length < 30 || bytes.readUInt32LE(4) + 8 !== bytes.length) invalid();
    let offset = 12, pixels = false;
    while (offset + 8 <= bytes.length) {
      const kind = bytes.toString("ascii", offset, offset + 4), size = bytes.readUInt32LE(offset + 4);
      const data = offset + 8, end = data + size;
      if (end > bytes.length) invalid();
      if (kind === "VP8 ") {
        if (size < 10 || !bytes.subarray(data + 3, data + 6).equals(Buffer.from([0x9d, 1, 0x2a]))) invalid();
        dimensions(bytes.readUInt16LE(data + 6) & 0x3fff, bytes.readUInt16LE(data + 8) & 0x3fff); pixels = true;
      } else if (kind === "VP8L") {
        if (size < 5 || bytes[data] !== 0x2f) invalid();
        const bits = bytes.readUInt32LE(data + 1);
        dimensions((bits & 0x3fff) + 1, ((bits >>> 14) & 0x3fff) + 1); pixels = true;
      } else if (kind === "VP8X") {
        if (size !== 10) invalid();
        dimensions(bytes.readUIntLE(data + 4, 3) + 1, bytes.readUIntLE(data + 7, 3) + 1);
      } else if (kind === "ANMF") {
        if (size < 24 || depth) invalid();
        dimensions(bytes.readUIntLE(data + 6, 3) + 1, bytes.readUIntLE(data + 9, 3) + 1);
        const frame = bytes.subarray(data + 16, end), header = Buffer.alloc(12);
        header.write("RIFF", 0); header.writeUInt32LE(frame.length + 4, 4); header.write("WEBP", 8);
        imageFormat(Buffer.concat([header, frame]), depth + 1); pixels = true;
      } else if (!["ICCP", "EXIF", "XMP ", "ALPH", "ANIM"].includes(kind)) invalid();
      offset = end + (size & 1);
    }
    if (!pixels || offset !== bytes.length) invalid();
    return { mime: "image/webp", extension: "webp" };
  }
  if (bytes.length > 4 && bytes[0] === 0xff && bytes[1] === 0xd8) {
    let offset = 2, pixels = false, header = false;
    while (offset < bytes.length) {
      if (bytes[offset++] !== 0xff) invalid();
      while (bytes[offset] === 0xff) offset++;
      const marker = bytes[offset++];
      if (marker === 0xd9) {
        if (!header || !pixels || offset !== bytes.length) invalid();
        return { mime: "image/jpeg", extension: "jpg" };
      }
      if (marker === undefined || marker === 0 || marker === 0xd8 || marker >= 0xd0 && marker <= 0xd7 || offset + 2 > bytes.length) invalid();
      const size = bytes.readUInt16BE(offset), end = offset + size;
      if (size < 2 || end > bytes.length) invalid();
      if ([0xc0,0xc1,0xc2,0xc3,0xc5,0xc6,0xc7,0xc9,0xca,0xcb,0xcd,0xce,0xcf].includes(marker)) {
        if (size < 8) invalid();
        dimensions(bytes.readUInt16BE(offset + 5), bytes.readUInt16BE(offset + 3)); header = true;
      }
      offset = end;
      if (marker === 0xda) {
        if (!header) invalid();
        pixels = true;
        // Entropy-coded bytes use FF00 escaping and optional restart markers.
        while (offset < bytes.length) {
          if (bytes[offset] !== 0xff) { offset++; continue; }
          const next = bytes[offset + 1];
          if (next === 0 || next >= 0xd0 && next <= 0xd7) { offset += 2; continue; }
          break;
        }
      }
    }
    invalid();
  }
  invalid();
}
