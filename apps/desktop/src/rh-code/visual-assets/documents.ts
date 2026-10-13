/** Asset documents are ordinary controller-owned text buffers. Canvas views
 * derive validated objects from those buffers; undo history never owns the
 * only copy of an edit. Native saving remains revisioned and hash checked. */
import {
  createPixel,
  validateModel,
  validatePixel,
  type PixelDocument,
  type VisualDocument,
} from "../../../../../code/visual-assets.mjs";

export type AssetKind = "pixel" | "model";
export const assetSuffix = (kind: AssetKind) =>
  kind === "pixel" ? ".rhtexture.json" : ".rhmodel.json";
export const serializeAsset = (value: VisualDocument) =>
  JSON.stringify(value, null, 2) + "\n";
export function parseAsset(kind: AssetKind, text: string): VisualDocument {
  const value: unknown = JSON.parse(text);
  return kind === "pixel" ? validatePixel(value) : validateModel(value);
}
export function assetPath(kind: AssetKind, value: string) {
  const suffix = assetSuffix(kind),
    path = value.trim().endsWith(suffix) ? value.trim() : value.trim() + suffix;
  if (
    !value.trim() ||
    path.startsWith("/") ||
    path.includes("\\") ||
    path.split("/").some((part) => !part || part === "." || part === "..") ||
    /[\u0000-\u001f\u007f]/.test(path)
  )
    throw new Error("Use a relative project path without parent directories");
  return path;
}
/** Nearest neighbour resizing preserves the palette and transparent pixels. */
export function resizeTexture(document: PixelDocument, size: number) {
  const next = createPixel(size, size);
  next.palette = [...document.palette];
  next.pixels = next.pixels.map((_, index) => {
    const x = index % size,
      y = Math.floor(index / size);
    return document.pixels[
      Math.floor((y * document.height) / size) * document.width +
        Math.floor((x * document.width) / size)
    ];
  });
  return validatePixel(next);
}
