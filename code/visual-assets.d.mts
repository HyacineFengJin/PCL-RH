export type Vec2 = [number, number];
export type Vec3 = [number, number, number];
export type FaceName = "north" | "south" | "east" | "west" | "up" | "down";
export type QuarterTurn = 0 | 90 | 180 | 270;
export interface PixelDocument {
  format: "pcl-rh-pixel";
  version: 1;
  width: number;
  height: number;
  /** Uppercase #RRGGBBAA entries; index zero is always #00000000. */
  palette: string[];
  /** Row major palette indices, with origin in the upper left. */
  pixels: number[];
}
export interface ModelFace {
  /** Minecraft texture coordinates, independent of texture dimensions: 0..16. */
  uv: [number, number, number, number];
  rotation?: QuarterTurn;
}
export interface ElementRotation {
  origin: Vec3;
  axis: "x" | "y" | "z";
  angle: -45 | -22.5 | 0 | 22.5 | 45;
  rescale?: boolean;
}
export interface ModelElement {
  id: string;
  from: Vec3;
  to: Vec3;
  faces: Partial<Record<FaceName, ModelFace>>;
  rotation?: ElementRotation;
}
export interface ModelDocument {
  format: "pcl-rh-model";
  version: 1;
  texture: PixelDocument;
  elements: ModelElement[];
}
export type VisualDocument = PixelDocument | ModelDocument;
export type PixelOperation =
  | { op: "brush"; points: Vec2[]; color: number }
  | {
      op: "rect";
      x: number;
      y: number;
      width: number;
      height: number;
      color: number;
      filled?: boolean;
    }
  | { op: "line"; from: Vec2; to: Vec2; color: number }
  | { op: "fill"; x: number; y: number; color: number }
  | { op: "palette"; index: number; color: string };
export type ModelOperation =
  | { op: "add"; element: ModelElement }
  | {
      op: "update";
      id: string;
      patch: Partial<Pick<ModelElement, "from" | "to" | "faces">> & {
        rotation?: ElementRotation | null;
      };
    }
  | { op: "remove"; id: string }
  | { op: "texture"; operations: PixelOperation[] };
export interface RgbaImage {
  width: number;
  height: number;
  data: Uint8ClampedArray;
}
export interface JavaModel {
  textures: { texture: string; particle: string };
  elements: Array<{
    from: Vec3;
    to: Vec3;
    faces: Partial<Record<FaceName, ModelFace & { texture: "#texture" }>>;
    rotation?: ElementRotation;
  }>;
}
export class VisualAssetError extends Error {
  readonly code: "INVALID_VISUAL_ASSET";
}
export const VISUAL_LIMITS: Readonly<{
  maxTextureDimension: 128;
  maxTexturePixels: 16384;
  maxPalette: 256;
  maxElements: 128;
  maxOperations: 256;
  maxImageDimension: 512;
  maxImagePixels: 262144;
  maxRasterSamples: 33554432;
}>;
export function validatePixel(value: unknown): PixelDocument;
export function validateModel(value: unknown): ModelDocument;
export function createPixel(width?: number, height?: number): PixelDocument;
export function createModel(): ModelDocument;
/** Every edit is atomic on a cloned document; failed batches leave their input intact. */
export function editPixel(
  document: PixelDocument,
  operations: PixelOperation[],
): PixelDocument;
export function editModel(
  document: ModelDocument,
  operations: ModelOperation[],
): ModelDocument;
export function renderPixel(
  document: PixelDocument,
  options?: { scale?: number; checker?: boolean },
): RgbaImage;
/** Bounded, orthographic, deterministic preview with nearest texture sampling. */
export function renderModel(
  document: ModelDocument,
  options?: { width?: number; height?: number; yaw?: number; pitch?: number },
): RgbaImage;
/** Export for Java block/item model JSON. textureResource is namespace:path without .png. */
export function exportJavaModel(
  document: ModelDocument,
  textureResource: string,
): JavaModel;
