import test from "node:test";
import assert from "node:assert/strict";
import { inflateSync } from "node:zlib";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import {
  VisualAssetError,
  VISUAL_LIMITS,
  createPixel,
  createModel,
  validatePixel,
  validateModel,
  editPixel,
  editModel,
  renderPixel,
  renderModel,
  exportJavaModel,
} from "../visual-assets.mjs";
import { encodePng } from "../visual-png.mjs";

const cloned = (value) => JSON.parse(JSON.stringify(value));
const hash = (image) => createHash("sha256").update(image.data).digest("hex");
const pixel = (image, x, y) =>
  Array.from(
    image.data.subarray(
      (y * image.width + x) * 4,
      (y * image.width + x) * 4 + 4,
    ),
  );
const opaque = (image) =>
  Array.from(image.data).filter((value, index) => index % 4 === 3 && value > 0)
    .length;
const invalid = (action) =>
  assert.throws(
    action,
    (error) =>
      error instanceof VisualAssetError &&
      error.code === "INVALID_VISUAL_ASSET",
  );

function coloredPixel(width = 4, height = 4) {
  const document = createPixel(width, height);
  document.palette.push("#FF0000FF", "#00FF00FF", "#0000FFFF", "#FFFF00FF");
  return document;
}

function flatElement(id, z, uv = [0, 0, 16, 16]) {
  return { id, from: [0, 0, z], to: [16, 16, z], faces: { south: { uv } } };
}

test("pixel schema owns a normalized clone and rejects opaque or unsupported document data", () => {
  const original = coloredPixel();
  original.palette[1] = "#fFaA00fF";
  const document = validatePixel(original);
  assert.equal(document.palette[1], "#FFAA00FF");
  document.pixels[0] = 1;
  assert.equal(original.pixels[0], 0);
  assert.notEqual(document.palette, original.palette);
  for (const document of [
    { ...original, name: "unsupported" },
    { ...original, version: 2 },
    { ...original, format: "pcl-rh-model" },
    { ...original, width: NaN },
    { ...original, height: Infinity },
    { ...original, width: 0 },
    { ...original, width: 129 },
    { ...original, width: 1.5 },
    { ...original, pixels: original.pixels.slice(1) },
    { ...original, pixels: [-1, ...original.pixels.slice(1)] },
    { ...original, pixels: [5, ...original.pixels.slice(1)] },
    { ...original, palette: ["#FF000000"] },
    { ...original, palette: ["#00000000", "#ffffff"] },
  ])
    invalid(() => validatePixel(document));
  const getter = { ...original };
  Object.defineProperty(getter, "width", {
    enumerable: true,
    get() {
      throw new Error("must not invoke accessor");
    },
  });
  invalid(() => validatePixel(getter));
  const sparse = { ...original, pixels: Array(16) };
  invalid(() => validatePixel(sparse));
  const hidden = { ...original };
  Object.defineProperty(hidden, "hidden", { value: true });
  invalid(() => validatePixel(hidden));
  invalid(() => validatePixel({ ...original, [Symbol("hidden")]: true }));
});

test("texture dimensions and palette indices enforce their boundary budgets", () => {
  const document = createPixel(128, 128);
  document.palette = ["#00000000", ...Array(255).fill("#FFFFFFFF")];
  document.pixels[16383] = 255;
  assert.equal(
    validatePixel(document).pixels.length,
    VISUAL_LIMITS.maxTexturePixels,
  );
  invalid(() =>
    editPixel(document, [{ op: "palette", index: 256, color: "#FF0000FF" }]),
  );
  invalid(() => createPixel(129, 1));
  invalid(() => createPixel(1, -1));
  invalid(() =>
    validatePixel({ ...document, palette: [...document.palette, "#FFFFFFFF"] }),
  );
});

test("pixel command batches are immutable, atomic and replay to the same document", () => {
  const original = createPixel(5, 5),
    before = cloned(original);
  const operations = [
    { op: "palette", index: 1, color: "#abcdef80" },
    { op: "rect", x: 0, y: 0, width: 5, height: 5, color: 1, filled: false },
    { op: "line", from: [1, 1], to: [3, 3], color: 1 },
    {
      op: "brush",
      points: [
        [3, 1],
        [1, 3],
      ],
      color: 1,
    },
  ];
  const edited = editPixel(original, operations);
  assert.deepEqual(edited, editPixel(original, cloned(operations)));
  assert.deepEqual(original, before);
  assert.equal(edited.palette[1], "#ABCDEF80");
  assert.equal(edited.pixels[12], 1);
  assert.equal(edited.pixels[7], 0);
  invalid(() =>
    editPixel(original, [
      ...operations,
      { op: "brush", points: [[5, 0]], color: 1 },
    ]),
  );
  assert.deepEqual(original, before);
  invalid(() =>
    editPixel(original, [{ op: "palette", index: 0, color: "#000000FF" }]),
  );
  invalid(() =>
    editPixel(original, [
      { op: "brush", points: [[0, 0]], color: 0, extra: true },
    ]),
  );
  invalid(() =>
    editPixel(original, [
      { op: "rect", x: 4, y: 4, width: 2, height: 1, color: 0 },
    ]),
  );
  invalid(() => editPixel(original, []));
  const maximum = Array.from({ length: 256 }, () => ({
    op: "brush",
    points: [[0, 0]],
    color: 0,
  }));
  assert.deepEqual(editPixel(original, maximum), original);
  invalid(() => editPixel(original, [...maximum, maximum[0]]));
});

test("flood fill respects four-connected regions and line endpoints in each direction", () => {
  const document = coloredPixel(7, 7);
  const outlined = editPixel(document, [
    { op: "rect", x: 1, y: 1, width: 5, height: 5, color: 1, filled: false },
  ]);
  const inside = editPixel(outlined, [{ op: "fill", x: 3, y: 3, color: 2 }]);
  assert.equal(inside.pixels.filter((color) => color === 2).length, 9);
  assert.equal(inside.pixels[0], 0);
  const outside = editPixel(inside, [{ op: "fill", x: 0, y: 0, color: 3 }]);
  assert.equal(outside.pixels.filter((color) => color === 3).length, 24);
  assert.deepEqual(
    editPixel(outside, [{ op: "fill", x: 0, y: 0, color: 3 }]),
    outside,
  );
  for (const [from, to] of [
    [
      [0, 0],
      [6, 6],
    ],
    [
      [6, 0],
      [0, 6],
    ],
    [
      [0, 3],
      [6, 3],
    ],
    [
      [3, 6],
      [3, 0],
    ],
    [
      [1, 1],
      [1, 1],
    ],
    [
      [0, 0],
      [2, 1],
    ],
    [
      [0, 0],
      [1, 2],
    ],
    [
      [0, 2],
      [1, 0],
    ],
    [
      [2, 0],
      [0, 1],
    ],
  ]) {
    const painted = editPixel(document, [{ op: "line", from, to, color: 4 }]);
    assert.equal(painted.pixels[from[1] * 7 + from[0]], 4);
    assert.equal(painted.pixels[to[1] * 7 + to[0]], 4);
    assert.deepEqual(
      painted,
      editPixel(document, [{ op: "line", from: to, to: from, color: 4 }]),
    );
  }
});

test("model schema preserves flat faces and rejects invalid geometry, UVs, rotations and identifiers", () => {
  const document = createModel(),
    normalized = validateModel(document);
  assert.notEqual(normalized.texture.pixels, document.texture.pixels);
  assert.notEqual(normalized.elements[0].from, document.elements[0].from);
  const flat = { ...document, elements: [flatElement("plane", 8)] };
  assert.equal(validateModel(flat).elements[0].faces.south.rotation, 0);
  for (const mutate of [
    (value) => value.elements.push(cloned(value.elements[0])),
    (value) => (value.elements[0].from[0] = -17),
    (value) => (value.elements[0].to[0] = 33),
    (value) => (value.elements[0].from[0] = 17),
    (value) => (value.elements[0].id = "../../escape"),
    (value) => (value.elements[0].name = "unsupported"),
    (value) => (value.elements[0].faces.north.uv[0] = -1),
    (value) => (value.elements[0].faces.north.uv[3] = 17),
    (value) => (value.elements[0].faces.north.rotation = 45),
    (value) => (value.elements[0].faces.north.texture = "other"),
    (value) => (value.elements[0].faces.extra = { uv: [0, 0, 16, 16] }),
    (value) =>
      (value.elements[0].rotation = {
        origin: [8, 8, 8],
        axis: "w",
        angle: 45,
      }),
    (value) =>
      (value.elements[0].rotation = {
        origin: [8, 8, 8],
        axis: "x",
        angle: 90,
      }),
    (value) =>
      (value.elements[0].rotation = {
        origin: [8, 8, 33],
        axis: "x",
        angle: 45,
      }),
    (value) =>
      (value.elements[0].rotation = {
        origin: [8, 8, 8],
        axis: "x",
        angle: 45,
        rescale: "true",
      }),
  ]) {
    const broken = cloned(document);
    mutate(broken);
    invalid(() => validateModel(broken));
  }
  const maximum = {
    ...document,
    elements: Array.from({ length: 128 }, (_, index) => ({
      ...cloned(document.elements[0]),
      id: `cube_${index}`,
    })),
  };
  assert.equal(validateModel(maximum).elements.length, 128);
  invalid(() =>
    validateModel({
      ...maximum,
      elements: [...maximum.elements, flatElement("overflow", 0)],
    }),
  );
});

test("model edits clone geometry, replace selected faces, remove rotations and edit embedded textures atomically", () => {
  const original = createModel(),
    before = cloned(original);
  const operations = [
    { op: "add", element: flatElement("plane", 8) },
    {
      op: "update",
      id: "cube",
      patch: {
        rotation: { origin: [8, 8, 8], axis: "y", angle: 22.5, rescale: true },
      },
    },
    {
      op: "texture",
      operations: [{ op: "brush", points: [[0, 0]], color: 2 }],
    },
    { op: "remove", id: "plane" },
  ];
  const edited = editModel(original, operations);
  assert.deepEqual(edited, editModel(original, cloned(operations)));
  assert.equal(edited.elements[0].rotation.rescale, true);
  assert.equal(edited.texture.pixels[0], 2);
  assert.deepEqual(original, before);
  const cleared = editModel(edited, [
    {
      op: "update",
      id: "cube",
      patch: { rotation: null, faces: { south: { uv: [16, 0, 0, 16] } } },
    },
  ]);
  assert.equal(cleared.elements[0].rotation, undefined);
  assert.deepEqual(Object.keys(cleared.elements[0].faces), ["south"]);
  invalid(() =>
    editModel(original, [...operations, { op: "remove", id: "unknown" }]),
  );
  assert.deepEqual(original, before);
  invalid(() =>
    editModel(original, [{ op: "add", element: original.elements[0] }]),
  );
  invalid(() => editModel(original, [{ op: "update", id: "cube", patch: {} }]));
  invalid(() =>
    editModel(original, [{ op: "update", id: "cube", patch: { id: "other" } }]),
  );
  assert.equal(
    editModel(original, [{ op: "remove", id: "cube" }]).elements.length,
    0,
  );
});

test("pixel rendering uses nearest scaling and preserves RGBA or composites a checker background", () => {
  const document = coloredPixel(2, 1);
  document.palette[2] = "#00FF0080";
  document.pixels = [1, 2];
  const image = renderPixel(document, { scale: 3 });
  assert.deepEqual([image.width, image.height], [6, 3]);
  assert.deepEqual(pixel(image, 0, 0), [255, 0, 0, 255]);
  assert.deepEqual(pixel(image, 2, 2), [255, 0, 0, 255]);
  assert.deepEqual(pixel(image, 3, 2), [0, 255, 0, 128]);
  const transparent = renderPixel(createPixel(2, 2));
  assert.equal(opaque(transparent), 0);
  const checker = renderPixel(document, { checker: true });
  assert.deepEqual(pixel(checker, 1, 0), [118, 246, 118, 255]);
  invalid(() => renderPixel(document, { scale: 1.5 }));
  invalid(() => renderPixel(createPixel(128, 128), { scale: 5 }));
  invalid(() => renderPixel(document, { checker: 1 }));
});

test("cuboid UV sampling, flips and clockwise quarter turns match visible texture corners", () => {
  const texture = coloredPixel();
  texture.pixels = texture.pixels.map((_, index) => {
    const x = index % 4,
      y = Math.floor(index / 4);
    return y < 2 ? (x < 2 ? 1 : 2) : x < 2 ? 3 : 4;
  });
  const model = {
    format: "pcl-rh-model",
    version: 1,
    texture,
    elements: [flatElement("plane", 8)],
  };
  const corners = (image) =>
    [
      [8, 8],
      [24, 8],
      [24, 24],
      [8, 24],
    ].map(([x, y]) => pixel(image, x, y).map((value) => (value > 0 ? 1 : 0)));
  const view = { width: 32, height: 32, yaw: 0, pitch: 0 };
  assert.deepEqual(corners(renderModel(model, view)), [
    [1, 0, 0, 1],
    [0, 1, 0, 1],
    [1, 1, 0, 1],
    [0, 0, 1, 1],
  ]);
  model.elements[0].faces.south.rotation = 90;
  assert.deepEqual(corners(renderModel(model, view)), [
    [0, 0, 1, 1],
    [1, 0, 0, 1],
    [0, 1, 0, 1],
    [1, 1, 0, 1],
  ]);
  model.elements[0].faces.south = { uv: [16, 0, 0, 16] };
  assert.deepEqual(corners(renderModel(model, view)), [
    [0, 1, 0, 1],
    [1, 0, 0, 1],
    [0, 0, 1, 1],
    [1, 1, 0, 1],
  ]);
});

test("z-buffer visibility is independent of opaque element order and transparent holes reveal the next surface", () => {
  const texture = coloredPixel(3, 1);
  texture.pixels = [1, 0, 2];
  const front = flatElement("front", 8, [0, 0, 16 / 3, 16]);
  const back = flatElement("back", 0, [32 / 3, 0, 16, 16]);
  const model = {
    format: "pcl-rh-model",
    version: 1,
    texture,
    elements: [front, back],
  };
  const view = { width: 32, height: 32, yaw: 0, pitch: 0 };
  const result = renderModel(model, view);
  assert.equal(pixel(result, 16, 16)[1], 0);
  assert.ok(pixel(result, 16, 16)[0] > 0);
  assert.equal(
    hash(result),
    hash(renderModel({ ...model, elements: [back, front] }, view)),
  );
  front.faces.south.uv = [16 / 3, 0, 32 / 3, 16];
  const throughHole = renderModel(model, view);
  assert.equal(pixel(throughHole, 16, 16)[0], 0);
  assert.ok(pixel(throughHole, 16, 16)[1] > 0);
});

test("translucent faces composite once along shared triangle edges", () => {
  const texture = createPixel(1, 1);
  texture.palette.push("#FF000080");
  texture.pixels[0] = 1;
  const model = {
    format: "pcl-rh-model",
    version: 1,
    texture,
    elements: [flatElement("plane", 8)],
  };
  const image = renderModel(model, { width: 32, height: 32, yaw: 0, pitch: 0 });
  assert.equal(pixel(image, 16, 16)[3], 128);
  assert.equal(pixel(image, 15, 16)[3], 128);
  assert.deepEqual(
    [
      ...new Set(Array.from(image.data).filter((_, index) => index % 4 === 3)),
    ].sort((a, b) => a - b),
    [0, 128],
  );
});

test("one-shot model rendering is deterministic, bounded and responds to element rotations", () => {
  const model = createModel(),
    first = renderModel(model),
    second = renderModel(cloned(model));
  assert.equal(hash(first), hash(second));
  assert.ok(opaque(first) > 1000 && opaque(first) < first.width * first.height);
  assert.deepEqual(pixel(first, 0, 0), [0, 0, 0, 0]);
  for (const axis of ["x", "y", "z"]) {
    const rotated = editModel(model, [
      {
        op: "update",
        id: "cube",
        patch: {
          rotation: { origin: [8, 8, 8], axis, angle: 22.5, rescale: true },
        },
      },
    ]);
    assert.notEqual(hash(first), hash(renderModel(rotated)));
  }
  assert.equal(opaque(renderModel({ ...model, elements: [] })), 0);
  invalid(() => renderModel(model, { width: 513 }));
  invalid(() => renderModel(model, { pitch: 91 }));
  invalid(() => renderModel(model, { yaw: NaN }));
  const overlapping = {
    ...model,
    elements: Array.from({ length: 128 }, (_, index) => ({
      ...cloned(model.elements[0]),
      id: `cube_${index}`,
    })),
  };
  invalid(() => renderModel(overlapping, { width: 512, height: 512 }));
});

test("Java block/item export emits owned geometry and texture references while refusing ambiguous resource paths", () => {
  const model = editModel(createModel(), [
    {
      op: "update",
      id: "cube",
      patch: {
        rotation: { origin: [8, 8, 8], axis: "y", angle: 45, rescale: true },
      },
    },
  ]);
  const output = exportJavaModel(model, "example:block/custom_texture");
  assert.deepEqual(output.textures, {
    texture: "example:block/custom_texture",
    particle: "example:block/custom_texture",
  });
  assert.equal(output.elements[0].id, undefined);
  assert.equal(output.elements[0].faces.north.texture, "#texture");
  assert.equal(output.elements[0].rotation.rescale, true);
  assert.deepEqual(output.elements[0].from, [0, 0, 0]);
  output.elements[0].from[0] = 4;
  output.elements[0].rotation.origin[0] = 4;
  assert.deepEqual(model.elements[0].from, [0, 0, 0]);
  assert.deepEqual(model.elements[0].rotation.origin, [8, 8, 8]);
  for (const resource of [
    "block/texture",
    "example:../secret",
    "example:block//texture",
    "Example:block/texture",
    "example:block/texture.png",
    "example:/texture",
  ])
    invalid(() => exportJavaModel(model, resource));
});

function independentCrc(bytes) {
  let result = 0xffffffff;
  for (const byte of bytes) {
    result ^= byte;
    for (let bit = 0; bit < 8; bit++)
      result = result & 1 ? (result >>> 1) ^ 0xedb88320 : result >>> 1;
  }
  return (result ^ 0xffffffff) >>> 0;
}

function decodePng(bytes) {
  assert.deepEqual(
    Array.from(bytes.subarray(0, 8)),
    [137, 80, 78, 71, 13, 10, 26, 10],
  );
  let offset = 8,
    width,
    height;
  const compressed = [],
    types = [];
  while (offset < bytes.length) {
    const length = bytes.readUInt32BE(offset),
      type = bytes.toString("ascii", offset + 4, offset + 8);
    const payload = bytes.subarray(offset + 8, offset + 8 + length);
    types.push(type);
    assert.equal(
      bytes.readUInt32BE(offset + 8 + length),
      independentCrc(bytes.subarray(offset + 4, offset + 8 + length)),
    );
    if (type === "IHDR") {
      width = payload.readUInt32BE(0);
      height = payload.readUInt32BE(4);
      assert.deepEqual(Array.from(payload.subarray(8)), [8, 6, 0, 0, 0]);
    } else if (type === "IDAT") compressed.push(payload);
    offset += length + 12;
  }
  assert.equal(offset, bytes.length);
  assert.deepEqual(types, ["IHDR", "IDAT", "IEND"]);
  const raw = inflateSync(Buffer.concat(compressed));
  assert.equal(raw.length, (width * 4 + 1) * height);
  const data = new Uint8ClampedArray(width * height * 4);
  for (let y = 0; y < height; y++) {
    assert.equal(raw[y * (width * 4 + 1)], 0);
    data.set(
      raw.subarray(y * (width * 4 + 1) + 1, (y + 1) * (width * 4 + 1)),
      y * width * 4,
    );
  }
  return { width, height, data };
}

test("PNG output independently decodes with valid CRCs and exact lossless RGBA", () => {
  const document = coloredPixel(3, 2);
  document.palette[2] = "#00FF0080";
  document.pixels = [0, 1, 2, 3, 4, 0];
  for (const image of [
    renderPixel(document),
    renderModel(createModel(), { width: 96, height: 80 }),
  ]) {
    const bytes = encodePng(image);
    assert.deepEqual(decodePng(bytes), image);
    assert.deepEqual(encodePng(image), bytes);
  }
  invalid(() => encodePng({ width: 0, height: 1, data: new Uint8Array(0) }));
  invalid(() => encodePng({ width: 1, height: 1, data: new Uint8Array(3) }));
  invalid(() => encodePng({ width: 1, height: 1, data: [0, 0, 0, 0] }));
  invalid(() =>
    encodePng({ width: 513, height: 1, data: new Uint8Array(513 * 4) }),
  );
});

test("shared native fixtures preserve exact texture RGBA, PNG and Java model semantics", () => {
  const fixtures = JSON.parse(
    readFileSync(
      new URL(
        "../../apps/desktop/src-tauri/src/rh_code/assets/fixtures.json",
        import.meta.url,
      ),
      "utf8",
    ),
  );
  assert.equal(fixtures.schemaVersion, 1);
  assert.equal(fixtures.textures.length, 2);
  assert.equal(fixtures.models.length, 2);
  const textures = new Map();
  for (const fixture of fixtures.textures) {
    const document = validatePixel(fixture.document);
    assert.deepEqual(document, fixture.document);
    assert.equal(fixture.rgbaRows.length, document.height);
    for (const row of fixture.rgbaRows) {
      assert.equal(row.length, document.width * 8);
      assert.match(row, /^[0-9a-f]+$/);
    }
    const expected = {
      width: document.width,
      height: document.height,
      data: new Uint8ClampedArray(
        Buffer.from(fixture.rgbaRows.join(""), "hex"),
      ),
    };
    assert.deepEqual(renderPixel(document), expected, fixture.name);
    assert.deepEqual(decodePng(encodePng(expected)), expected, fixture.name);
    textures.set(fixture.name, document);
  }
  for (const fixture of fixtures.models) {
    assert.ok(textures.has(fixture.texture), fixture.name);
    const document = validateModel({
      format: "pcl-rh-model",
      version: 1,
      texture: textures.get(fixture.texture),
      elements: fixture.elements,
    });
    assert.deepEqual(
      exportJavaModel(document, fixtures.textureResource),
      fixture.javaModel,
      fixture.name,
    );
  }
});
