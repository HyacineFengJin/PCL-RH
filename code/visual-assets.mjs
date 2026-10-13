/**
 * Browser/Node shared document engine for small Minecraft textures and cuboids.
 * Documents contain JSON data only. Validation owns the normalized clone, and
 * edits never publish a partially valid batch. Rendering is a bounded one-shot
 * CPU operation; no DOM, process, filesystem or continuously running scene is
 * involved. Java export covers block/item model geometry, not entity rigs.
 */
export const VISUAL_LIMITS = Object.freeze({
  maxTextureDimension: 128,
  maxTexturePixels: 16384,
  maxPalette: 256,
  maxElements: 128,
  maxOperations: 256,
  maxImageDimension: 512,
  maxImagePixels: 262144,
  maxRasterSamples: 33554432,
});

const FACE_NAMES = ["north", "south", "east", "west", "up", "down"];
const QUARTER_TURNS = [0, 90, 180, 270];
const ELEMENT_ANGLES = [-45, -22.5, 0, 22.5, 45];
const TRANSPARENT = "#00000000";

export class VisualAssetError extends Error {
  constructor(message) {
    super(message);
    this.name = "VisualAssetError";
    this.code = "INVALID_VISUAL_ASSET";
  }
}

function requireValue(condition, message) {
  if (!condition) throw new VisualAssetError(message);
}

function record(value, path, required, optional = []) {
  requireValue(
    value !== null &&
      typeof value === "object" &&
      !Array.isArray(value) &&
      (Object.getPrototypeOf(value) === Object.prototype ||
        Object.getPrototypeOf(value) === null),
    `${path} must be a plain JSON object`,
  );
  const allowed = new Set([...required, ...optional]);
  // Reject accessors and hidden/symbol fields too: callers cannot smuggle data
  // that serializes differently from the document actually being validated.
  for (const key of Reflect.ownKeys(value)) {
    requireValue(
      typeof key === "string" && allowed.has(key),
      `${path} has an unsupported field`,
    );
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    requireValue(
      descriptor.enumerable && "value" in descriptor,
      `${path}.${key} must be JSON data`,
    );
  }
  for (const key of required)
    requireValue(Object.hasOwn(value, key), `${path}.${key} is required`);
  return value;
}

function array(value, path, minimum, maximum) {
  requireValue(
    Array.isArray(value) && value.length >= minimum && value.length <= maximum,
    `${path} must contain ${minimum}..${maximum} entries`,
  );
  requireValue(
    Reflect.ownKeys(value).length === value.length + 1,
    `${path} must be a dense JSON array`,
  );
  for (let index = 0; index < value.length; index++) {
    const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
    requireValue(
      descriptor?.enumerable && "value" in descriptor,
      `${path} must be a dense JSON array`,
    );
  }
  return value;
}

function number(value, path, minimum, maximum, integer = false) {
  requireValue(
    typeof value === "number" &&
      Number.isFinite(value) &&
      value >= minimum &&
      value <= maximum &&
      (!integer || Number.isInteger(value)),
    `${path} must be ${integer ? "an integer" : "finite"} in ${minimum}..${maximum}`,
  );
  return Object.is(value, -0) ? 0 : value;
}

function boolean(value, path) {
  requireValue(typeof value === "boolean", `${path} must be a boolean`);
  return value;
}

function vector(value, path, length, minimum, maximum, integer = false) {
  return array(value, path, length, length).map((entry, index) =>
    number(entry, `${path}[${index}]`, minimum, maximum, integer),
  );
}

function paletteColor(value, path) {
  requireValue(
    typeof value === "string" && /^#[0-9a-fA-F]{8}$/.test(value),
    `${path} must be #RRGGBBAA`,
  );
  return value.toUpperCase();
}

function elementId(value, path) {
  requireValue(
    typeof value === "string" && /^[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}$/.test(value),
    `${path} must be a 1..64 character element identifier`,
  );
  return value;
}

export function validatePixel(value) {
  record(value, "pixel", [
    "format",
    "version",
    "width",
    "height",
    "palette",
    "pixels",
  ]);
  requireValue(
    value.format === "pcl-rh-pixel" && value.version === 1,
    "Unsupported pixel document format or version",
  );
  const width = number(
    value.width,
    "pixel.width",
    1,
    VISUAL_LIMITS.maxTextureDimension,
    true,
  );
  const height = number(
    value.height,
    "pixel.height",
    1,
    VISUAL_LIMITS.maxTextureDimension,
    true,
  );
  requireValue(
    width * height <= VISUAL_LIMITS.maxTexturePixels,
    "Texture pixel budget exceeded",
  );
  const palette = array(
    value.palette,
    "pixel.palette",
    1,
    VISUAL_LIMITS.maxPalette,
  ).map((color, index) => paletteColor(color, `pixel.palette[${index}]`));
  requireValue(
    palette[0] === TRANSPARENT,
    "Palette index zero must be #00000000",
  );
  const pixels = array(
    value.pixels,
    "pixel.pixels",
    width * height,
    width * height,
  ).map((color, index) =>
    number(color, `pixel.pixels[${index}]`, 0, palette.length - 1, true),
  );
  return { format: "pcl-rh-pixel", version: 1, width, height, palette, pixels };
}

function validateRotation(value, path) {
  record(value, path, ["origin", "axis", "angle"], ["rescale"]);
  const origin = vector(value.origin, `${path}.origin`, 3, -16, 32);
  requireValue(
    ["x", "y", "z"].includes(value.axis),
    `${path}.axis must be x, y or z`,
  );
  requireValue(
    ELEMENT_ANGLES.includes(value.angle),
    `${path}.angle is unsupported`,
  );
  const rescale = Object.hasOwn(value, "rescale")
    ? boolean(value.rescale, `${path}.rescale`)
    : false;
  return { origin, axis: value.axis, angle: value.angle, rescale };
}

function validateElement(value, path) {
  record(value, path, ["id", "from", "to", "faces"], ["rotation"]);
  const id = elementId(value.id, `${path}.id`);
  const from = vector(value.from, `${path}.from`, 3, -16, 32);
  const to = vector(value.to, `${path}.to`, 3, -16, 32);
  requireValue(
    from.every((coordinate, axis) => coordinate <= to[axis]),
    `${path}.from must not exceed to`,
  );
  // Zero thickness is accepted for Minecraft's commonly used flat geometry.
  record(value.faces, `${path}.faces`, [], FACE_NAMES);
  const faces = {};
  for (const name of FACE_NAMES) {
    if (!Object.hasOwn(value.faces, name)) continue;
    const face = record(
      value.faces[name],
      `${path}.faces.${name}`,
      ["uv"],
      ["rotation"],
    );
    const uv = vector(face.uv, `${path}.faces.${name}.uv`, 4, 0, 16);
    const rotation = Object.hasOwn(face, "rotation") ? face.rotation : 0;
    requireValue(
      QUARTER_TURNS.includes(rotation),
      `${path}.faces.${name}.rotation is unsupported`,
    );
    faces[name] = { uv, rotation };
  }
  const result = { id, from, to, faces };
  if (Object.hasOwn(value, "rotation"))
    result.rotation = validateRotation(value.rotation, `${path}.rotation`);
  return result;
}

export function validateModel(value) {
  record(value, "model", ["format", "version", "texture", "elements"]);
  requireValue(
    value.format === "pcl-rh-model" && value.version === 1,
    "Unsupported model document format or version",
  );
  const texture = validatePixel(value.texture);
  const elements = array(
    value.elements,
    "model.elements",
    0,
    VISUAL_LIMITS.maxElements,
  ).map((element, index) =>
    validateElement(element, `model.elements[${index}]`),
  );
  requireValue(
    new Set(elements.map((element) => element.id)).size === elements.length,
    "Model element identifiers must be unique",
  );
  return { format: "pcl-rh-model", version: 1, texture, elements };
}

export function createPixel(width = 16, height = 16) {
  number(width, "width", 1, VISUAL_LIMITS.maxTextureDimension, true);
  number(height, "height", 1, VISUAL_LIMITS.maxTextureDimension, true);
  return {
    format: "pcl-rh-pixel",
    version: 1,
    width,
    height,
    palette: [TRANSPARENT],
    pixels: Array(width * height).fill(0),
  };
}

export function createModel() {
  const texture = createPixel();
  texture.palette.push("#8B75CFFF", "#BFAAEDFF", "#65549AFF");
  texture.pixels = texture.pixels.map((_, index) => {
    const x = index % 16,
      y = Math.floor(index / 16);
    return x === 0 || y === 0 || x === 15 || y === 15
      ? 3
      : (x + y) % 4 === 0
        ? 2
        : 1;
  });
  return {
    format: "pcl-rh-model",
    version: 1,
    texture,
    elements: [
      {
        id: "cube",
        from: [0, 0, 0],
        to: [16, 16, 16],
        faces: Object.fromEntries(
          FACE_NAMES.map((name) => [name, { uv: [0, 0, 16, 16], rotation: 0 }]),
        ),
      },
    ],
  };
}

function operationBatch(operations, path) {
  return array(operations, path, 1, VISUAL_LIMITS.maxOperations);
}

function pixelPoint(value, path, document) {
  const point = vector(
    value,
    path,
    2,
    0,
    VISUAL_LIMITS.maxTextureDimension - 1,
    true,
  );
  requireValue(
    point[0] < document.width && point[1] < document.height,
    `${path} lies outside the texture`,
  );
  return point;
}

function paintLine(document, from, to, color) {
  // Canonical endpoint order makes the tie case choose the same pixel whether
  // a user draws the segment forward or backward.
  if (from[0] > to[0] || (from[0] === to[0] && from[1] > to[1]))
    [from, to] = [to, from];
  let [x, y] = from;
  const [targetX, targetY] = to;
  const dx = Math.abs(targetX - x),
    dy = -Math.abs(targetY - y);
  const sx = x < targetX ? 1 : -1,
    sy = y < targetY ? 1 : -1;
  let error = dx + dy;
  for (;;) {
    document.pixels[y * document.width + x] = color;
    if (x === targetX && y === targetY) return;
    const doubled = 2 * error;
    if (doubled >= dy) {
      error += dy;
      x += sx;
    }
    if (doubled <= dx) {
      error += dx;
      y += sy;
    }
  }
}

function floodFill(document, x, y, color) {
  const start = y * document.width + x,
    previous = document.pixels[start];
  if (previous === color) return;
  // Mark on insertion: each pixel appears in the queue at most once, including
  // narrow regions and closed rings. Queue memory cannot exceed texture size.
  const queue = new Uint32Array(document.pixels.length);
  let first = 0,
    last = 1;
  queue[0] = start;
  document.pixels[start] = color;
  const enqueue = (index) => {
    if (document.pixels[index] === previous) {
      document.pixels[index] = color;
      queue[last++] = index;
    }
  };
  while (first < last) {
    const index = queue[first++],
      column = index % document.width;
    if (column > 0) enqueue(index - 1);
    if (column < document.width - 1) enqueue(index + 1);
    if (index >= document.width) enqueue(index - document.width);
    if (index + document.width < document.pixels.length)
      enqueue(index + document.width);
  }
}

function applyPixelOperation(document, operation, path) {
  record(
    operation,
    path,
    ["op"],
    [
      "points",
      "color",
      "x",
      "y",
      "width",
      "height",
      "filled",
      "from",
      "to",
      "index",
    ],
  );
  if (operation.op === "palette") {
    record(operation, path, ["op", "index", "color"]);
    const index = number(
      operation.index,
      `${path}.index`,
      0,
      document.palette.length,
      true,
    );
    const color = paletteColor(operation.color, `${path}.color`);
    requireValue(
      index < VISUAL_LIMITS.maxPalette,
      "Palette entry budget exceeded",
    );
    requireValue(
      index !== 0 || color === TRANSPARENT,
      "Palette index zero must remain transparent",
    );
    document.palette[index] = color;
    return;
  }
  const color = number(
    operation.color,
    `${path}.color`,
    0,
    document.palette.length - 1,
    true,
  );
  if (operation.op === "brush") {
    record(operation, path, ["op", "points", "color"]);
    const points = array(
      operation.points,
      `${path}.points`,
      1,
      VISUAL_LIMITS.maxTexturePixels,
    );
    for (let index = 0; index < points.length; index++) {
      const [x, y] = pixelPoint(
        points[index],
        `${path}.points[${index}]`,
        document,
      );
      document.pixels[y * document.width + x] = color;
    }
  } else if (operation.op === "line") {
    record(operation, path, ["op", "from", "to", "color"]);
    paintLine(
      document,
      pixelPoint(operation.from, `${path}.from`, document),
      pixelPoint(operation.to, `${path}.to`, document),
      color,
    );
  } else if (operation.op === "rect") {
    record(
      operation,
      path,
      ["op", "x", "y", "width", "height", "color"],
      ["filled"],
    );
    const [x, y] = pixelPoint([operation.x, operation.y], path, document);
    const width = number(
      operation.width,
      `${path}.width`,
      1,
      document.width - x,
      true,
    );
    const height = number(
      operation.height,
      `${path}.height`,
      1,
      document.height - y,
      true,
    );
    const filled = Object.hasOwn(operation, "filled")
      ? boolean(operation.filled, `${path}.filled`)
      : true;
    for (let row = y; row < y + height; row++)
      for (let column = x; column < x + width; column++)
        if (
          filled ||
          row === y ||
          column === x ||
          row === y + height - 1 ||
          column === x + width - 1
        )
          document.pixels[row * document.width + column] = color;
  } else if (operation.op === "fill") {
    record(operation, path, ["op", "x", "y", "color"]);
    const [x, y] = pixelPoint([operation.x, operation.y], path, document);
    floodFill(document, x, y, color);
  } else {
    throw new VisualAssetError(`${path}.op is unsupported`);
  }
}

export function editPixel(value, operations) {
  const document = validatePixel(value);
  operationBatch(operations, "operations").forEach((operation, index) =>
    applyPixelOperation(document, operation, `operations[${index}]`),
  );
  return document;
}

export function editModel(value, operations) {
  const document = validateModel(value);
  operationBatch(operations, "operations").forEach((operation, index) => {
    const path = `operations[${index}]`;
    record(operation, path, ["op"], ["element", "id", "patch", "operations"]);
    if (operation.op === "texture") {
      record(operation, path, ["op", "operations"]);
      document.texture = editPixel(document.texture, operation.operations);
      return;
    }
    if (operation.op === "add") {
      record(operation, path, ["op", "element"]);
      requireValue(
        document.elements.length < VISUAL_LIMITS.maxElements,
        "Model element budget exceeded",
      );
      const element = validateElement(operation.element, `${path}.element`);
      requireValue(
        !document.elements.some((candidate) => candidate.id === element.id),
        "Duplicate model element identifier",
      );
      document.elements.push(element);
      return;
    }
    requireValue(
      operation.op === "update" || operation.op === "remove",
      `${path}.op is unsupported`,
    );
    record(
      operation,
      path,
      operation.op === "remove" ? ["op", "id"] : ["op", "id", "patch"],
    );
    const id = elementId(operation.id, `${path}.id`);
    const elementIndex = document.elements.findIndex(
      (element) => element.id === id,
    );
    requireValue(elementIndex !== -1, `Unknown model element: ${id}`);
    if (operation.op === "remove") {
      document.elements.splice(elementIndex, 1);
      return;
    }
    const patch = record(
      operation.patch,
      `${path}.patch`,
      [],
      ["from", "to", "faces", "rotation"],
    );
    requireValue(
      Object.keys(patch).length > 0,
      `${path}.patch must not be empty`,
    );
    const updated = { ...document.elements[elementIndex], ...patch };
    if (patch.rotation === null) delete updated.rotation;
    document.elements[elementIndex] = validateElement(
      updated,
      `${path}.element`,
    );
  });
  return document;
}

function imageDimensions(width, height) {
  number(width, "image.width", 1, VISUAL_LIMITS.maxImageDimension, true);
  number(height, "image.height", 1, VISUAL_LIMITS.maxImageDimension, true);
  requireValue(
    width * height <= VISUAL_LIMITS.maxImagePixels,
    "Image pixel budget exceeded",
  );
}

function rgbaPalette(document) {
  return document.palette.map((color) =>
    [1, 3, 5, 7].map((offset) => parseInt(color.slice(offset, offset + 2), 16)),
  );
}

export function renderPixel(value, options = {}) {
  const document = validatePixel(value);
  record(options, "options", [], ["scale", "checker"]);
  const scale = number(options.scale ?? 1, "options.scale", 1, 32, true);
  const checker = Object.hasOwn(options, "checker")
    ? boolean(options.checker, "options.checker")
    : false;
  const width = document.width * scale,
    height = document.height * scale;
  imageDimensions(width, height);
  const data = new Uint8ClampedArray(width * height * 4),
    palette = rgbaPalette(document);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const color =
        palette[
          document.pixels[
            Math.floor(y / scale) * document.width + Math.floor(x / scale)
          ]
        ];
      const index = (y * width + x) * 4;
      if (checker) {
        const background =
          (Math.floor(x / (4 * scale)) + Math.floor(y / (4 * scale))) % 2
            ? 206
            : 237;
        const alpha = color[3] / 255;
        for (let channel = 0; channel < 3; channel++)
          data[index + channel] =
            color[channel] * alpha + background * (1 - alpha);
        data[index + 3] = 255;
      } else {
        data.set(color, index);
      }
    }
  }
  return { width, height, data };
}

function subtract(left, right) {
  return left.map((value, index) => value - right[index]);
}
function dot(left, right) {
  return left.reduce(
    (result, value, index) => result + value * right[index],
    0,
  );
}
function cross(left, right) {
  return [
    left[1] * right[2] - left[2] * right[1],
    left[2] * right[0] - left[0] * right[2],
    left[0] * right[1] - left[1] * right[0],
  ];
}

function rotatePoint(point, rotation) {
  if (!rotation || rotation.angle === 0) return [...point];
  const relative = subtract(point, rotation.origin);
  const axis = { x: 0, y: 1, z: 2 }[rotation.axis];
  const first = (axis + 1) % 3,
    second = (axis + 2) % 3;
  const radians = (rotation.angle * Math.PI) / 180,
    cosine = Math.cos(radians),
    sine = Math.sin(radians);
  const a = relative[first],
    b = relative[second];
  const rescale = rotation.rescale ? 1 / cosine : 1;
  relative[first] = (a * cosine - b * sine) * rescale;
  relative[second] = (a * sine + b * cosine) * rescale;
  return relative.map(
    (coordinate, index) => coordinate + rotation.origin[index],
  );
}

function cuboidFaces(element) {
  const [x0, y0, z0] = element.from,
    [x1, y1, z1] = element.to;
  // The four vertices are top left, top right, bottom right, bottom left when
  // each Minecraft face is viewed from outside the block. UVs share that order.
  const vertices = {
    north: [
      [x1, y1, z0],
      [x0, y1, z0],
      [x0, y0, z0],
      [x1, y0, z0],
    ],
    south: [
      [x0, y1, z1],
      [x1, y1, z1],
      [x1, y0, z1],
      [x0, y0, z1],
    ],
    east: [
      [x1, y1, z1],
      [x1, y1, z0],
      [x1, y0, z0],
      [x1, y0, z1],
    ],
    west: [
      [x0, y1, z0],
      [x0, y1, z1],
      [x0, y0, z1],
      [x0, y0, z0],
    ],
    up: [
      [x0, y1, z0],
      [x1, y1, z0],
      [x1, y1, z1],
      [x0, y1, z1],
    ],
    down: [
      [x0, y0, z1],
      [x1, y0, z1],
      [x1, y0, z0],
      [x0, y0, z0],
    ],
  };
  return FACE_NAMES.filter((name) => element.faces[name]).map((name) => {
    const points = vertices[name].map((point) =>
      rotatePoint(point, element.rotation),
    );
    const [u1, v1, u2, v2] = element.faces[name].uv;
    const corners = [
      [u1, v1],
      [u2, v1],
      [u2, v2],
      [u1, v2],
    ];
    const turns = element.faces[name].rotation / 90;
    // A positive Minecraft face rotation turns the displayed texture clockwise.
    const uv = corners.map((_, index) => corners[(index + 4 - turns) % 4]);
    return { points, uv };
  });
}

function edge(a, b, x, y) {
  return (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
}

function triangle(vertices, brightness) {
  let [a, b, c] = vertices;
  let area = edge(a, b, c[0], c[1]);
  if (Math.abs(area) < 1e-9) return null;
  if (area < 0) {
    [b, c] = [c, b];
    area = -area;
  }
  return {
    vertices: [a, b, c],
    area,
    brightness,
    depth: (a[2] + b[2] + c[2]) / 3,
  };
}

function triangleBounds(item, width, height) {
  const points = item.vertices;
  return [
    Math.max(0, Math.ceil(Math.min(...points.map((point) => point[0])) - 0.5)),
    Math.max(0, Math.ceil(Math.min(...points.map((point) => point[1])) - 0.5)),
    Math.min(
      width - 1,
      Math.floor(Math.max(...points.map((point) => point[0])) - 0.5),
    ),
    Math.min(
      height - 1,
      Math.floor(Math.max(...points.map((point) => point[1])) - 0.5),
    ),
  ];
}

function topLeft(a, b) {
  const dy = b[1] - a[1],
    dx = b[0] - a[0];
  return dy < 0 || (dy === 0 && dx > 0);
}

function rasterize(item, image, depthBuffer, texture, palette, translucent) {
  const [a, b, c] = item.vertices;
  const [minX, minY, maxX, maxY] = item.bounds;
  const inclusive = [topLeft(b, c), topLeft(c, a), topLeft(a, b)];
  for (let y = minY; y <= maxY; y++) {
    for (let x = minX; x <= maxX; x++) {
      const px = x + 0.5,
        py = y + 0.5;
      const weights = [
        edge(b, c, px, py),
        edge(c, a, px, py),
        edge(a, b, px, py),
      ];
      if (
        weights.some(
          (weight, index) =>
            weight < -1e-9 || (Math.abs(weight) <= 1e-9 && !inclusive[index]),
        )
      )
        continue;
      const [wa, wb, wc] = weights.map((weight) => weight / item.area);
      const depth = a[2] * wa + b[2] * wb + c[2] * wc;
      const pixel = y * image.width + x;
      if (depth < depthBuffer[pixel] - 1e-9) continue;
      const u = a[3] * wa + b[3] * wb + c[3] * wc;
      const v = a[4] * wa + b[4] * wb + c[4] * wc;
      const tx = Math.max(
        0,
        Math.min(texture.width - 1, Math.floor((u / 16) * texture.width)),
      );
      const ty = Math.max(
        0,
        Math.min(texture.height - 1, Math.floor((v / 16) * texture.height)),
      );
      const color = palette[texture.pixels[ty * texture.width + tx]],
        alpha = color[3];
      if (alpha === 0 || (translucent ? alpha === 255 : alpha !== 255))
        continue;
      const offset = pixel * 4;
      if (!translucent) {
        for (let channel = 0; channel < 3; channel++)
          image.data[offset + channel] = color[channel] * item.brightness;
        image.data[offset + 3] = 255;
        depthBuffer[pixel] = depth;
      } else {
        const sourceAlpha = alpha / 255,
          destinationAlpha = image.data[offset + 3] / 255;
        const resultAlpha = sourceAlpha + destinationAlpha * (1 - sourceAlpha);
        for (let channel = 0; channel < 3; channel++)
          image.data[offset + channel] =
            (color[channel] * item.brightness * sourceAlpha +
              image.data[offset + channel] *
                destinationAlpha *
                (1 - sourceAlpha)) /
            resultAlpha;
        image.data[offset + 3] = resultAlpha * 255;
      }
    }
  }
}

export function renderModel(value, options = {}) {
  const document = validateModel(value);
  record(options, "options", [], ["width", "height", "yaw", "pitch"]);
  const width = options.width ?? 192,
    height = options.height ?? 192;
  imageDimensions(width, height);
  const yaw =
    (number(options.yaw ?? 45, "options.yaw", -360, 360) * Math.PI) / 180;
  const pitch =
    (number(options.pitch ?? 30, "options.pitch", -90, 90) * Math.PI) / 180;
  const right = [Math.cos(yaw), 0, -Math.sin(yaw)];
  const up = [
    -Math.sin(yaw) * Math.sin(pitch),
    Math.cos(pitch),
    -Math.cos(yaw) * Math.sin(pitch),
  ];
  const towardCamera = [
    Math.sin(yaw) * Math.cos(pitch),
    Math.sin(pitch),
    Math.cos(yaw) * Math.cos(pitch),
  ];
  const image = {
    width,
    height,
    data: new Uint8ClampedArray(width * height * 4),
  };
  const faces = document.elements.flatMap(cuboidFaces);
  if (!faces.length) return image;
  const project = (point) => [
    dot(point, right),
    -dot(point, up),
    dot(point, towardCamera),
  ];
  const projected = faces.flatMap((face) => face.points.map(project));
  const minX = Math.min(...projected.map((point) => point[0])),
    maxX = Math.max(...projected.map((point) => point[0]));
  const minY = Math.min(...projected.map((point) => point[1])),
    maxY = Math.max(...projected.map((point) => point[1]));
  const scale = Math.min(
    (width * 0.82) / Math.max(maxX - minX, 1),
    (height * 0.82) / Math.max(maxY - minY, 1),
  );
  const centerX = (minX + maxX) / 2,
    centerY = (minY + maxY) / 2;
  const light = [0.35, 0.8, 0.45],
    lightLength = Math.hypot(...light);
  const triangles = [];
  for (const face of faces) {
    const normal = cross(
      subtract(face.points[2], face.points[0]),
      subtract(face.points[1], face.points[0]),
    );
    const normalLength = Math.hypot(...normal);
    if (normalLength < 1e-9 || dot(normal, towardCamera) <= 1e-9) continue;
    const brightness =
      0.58 +
      0.42 * Math.max(0, dot(normal, light) / (normalLength * lightLength));
    const vertices = face.points.map((point, index) => {
      const [x, y, depth] = project(point);
      return [
        (x - centerX) * scale + width / 2,
        (y - centerY) * scale + height / 2,
        depth,
        ...face.uv[index],
      ];
    });
    for (const indices of [
      [0, 1, 2],
      [0, 2, 3],
    ]) {
      const item = triangle(
        indices.map((index) => vertices[index]),
        brightness,
      );
      if (item) {
        item.bounds = triangleBounds(item, width, height);
        triangles.push(item);
      }
    }
  }
  // Count both opaque/translucent passes before allocating or drawing per-face
  // work. Pathological overlapping models fail deterministically at admission.
  const samples = triangles.reduce((sum, item) => {
    const [x0, y0, x1, y1] = item.bounds;
    return sum + Math.max(0, x1 - x0 + 1) * Math.max(0, y1 - y0 + 1) * 2;
  }, 0);
  requireValue(
    samples <= VISUAL_LIMITS.maxRasterSamples,
    "Model preview raster budget exceeded; choose a smaller image",
  );
  const depthBuffer = new Float64Array(width * height).fill(-Infinity);
  const palette = rgbaPalette(document.texture);
  for (const item of triangles)
    rasterize(item, image, depthBuffer, document.texture, palette, false);
  if (palette.some((color) => color[3] > 0 && color[3] < 255))
    for (const item of [...triangles].sort(
      (left, right) => left.depth - right.depth,
    ))
      rasterize(item, image, depthBuffer, document.texture, palette, true);
  return image;
}

export function exportJavaModel(value, textureResource) {
  const document = validateModel(value);
  requireValue(
    typeof textureResource === "string" &&
      textureResource.length <= 240 &&
      /^[a-z0-9_.-]+:[a-z0-9_./-]+$/.test(textureResource) &&
      !textureResource.endsWith(".png") &&
      textureResource
        .split(":")[1]
        .split("/")
        .every((part) => part.length > 0 && part !== "." && part !== ".."),
    "Texture resource must be namespace:path without .png or traversal segments",
  );
  return {
    textures: { texture: textureResource, particle: textureResource },
    elements: document.elements.map((element) => {
      const result = {
        from: [...element.from],
        to: [...element.to],
        faces: Object.fromEntries(
          Object.entries(element.faces).map(([name, face]) => [
            name,
            { uv: [...face.uv], rotation: face.rotation, texture: "#texture" },
          ]),
        ),
      };
      if (element.rotation)
        result.rotation = {
          ...element.rotation,
          origin: [...element.rotation.origin],
        };
      return result;
    }),
  };
}
