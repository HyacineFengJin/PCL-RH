/** Node-only lossless PNG encoder for small AI preview/export images. Keep this
 * out of browser imports: the shared document/rendering engine has no Node API. */
import { deflateSync } from "node:zlib";
import { VISUAL_LIMITS, VisualAssetError } from "./visual-assets.mjs";

const CRC_TABLE = Uint32Array.from({ length: 256 }, (_, byte) => {
  let result = byte;
  for (let bit = 0; bit < 8; bit++)
    result = result & 1 ? 0xedb88320 ^ (result >>> 1) : result >>> 1;
  return result >>> 0;
});

function crc32(bytes) {
  let result = 0xffffffff;
  for (const byte of bytes)
    result = CRC_TABLE[(result ^ byte) & 255] ^ (result >>> 8);
  return (result ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const name = Buffer.from(type, "ascii"),
    output = Buffer.alloc(12 + data.length);
  output.writeUInt32BE(data.length, 0);
  name.copy(output, 4);
  data.copy(output, 8);
  output.writeUInt32BE(
    crc32(output.subarray(4, 8 + data.length)),
    8 + data.length,
  );
  return output;
}

export function encodePng(image) {
  const width = image?.width,
    height = image?.height,
    data = image?.data;
  if (
    !Number.isInteger(width) ||
    !Number.isInteger(height) ||
    width < 1 ||
    height < 1 ||
    width > VISUAL_LIMITS.maxImageDimension ||
    height > VISUAL_LIMITS.maxImageDimension ||
    width * height > VISUAL_LIMITS.maxImagePixels ||
    !(data instanceof Uint8Array || data instanceof Uint8ClampedArray) ||
    data.length !== width * height * 4
  )
    throw new VisualAssetError(
      "PNG requires a bounded image with exact RGBA bytes",
    );
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8; // Eight bits per RGBA channel; color type 6, no interlace.
  header[9] = 6;
  const rows = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y++)
    rows.set(
      data.subarray(y * width * 4, (y + 1) * width * 4),
      y * (width * 4 + 1) + 1,
    );
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(rows, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}
