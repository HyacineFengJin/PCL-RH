/** Declarative visual tools. The host service owns drafts, revision recovery,
 * rendering and review publication; models supply bounded structured edits. */
import { check, throwIfAborted } from "./errors.mjs";
import { sourcePath } from "../../../code/harness-snapshot.mjs";
import { validateModel } from "../../../code/visual-assets.mjs";

const integer = (minimum, maximum) => ({ type: "integer", minimum, maximum });
const object = (properties, required = Object.keys(properties)) => ({
  type: "object",
  properties,
  required,
  additionalProperties: false,
});
const tuple = (length, values) => ({
  type: "array",
  minItems: length,
  maxItems: length,
  items: values,
});
const color = {
  type: "string",
  pattern: "^#[0-9A-F]{8}$",
  description: "RGBA hexadecimal; index zero stays #00000000.",
};
const point = tuple(2, integer(0, 127));
const vector = tuple(3, { type: "number", minimum: -16, maximum: 32 });
const face = object(
  {
    uv: tuple(4, { type: "number", minimum: 0, maximum: 16 }),
    rotation: { enum: [0, 90, 180, 270] },
  },
  ["uv"],
);
const faces = object(
  Object.fromEntries(
    ["north", "south", "east", "west", "up", "down"].map((name) => [
      name,
      face,
    ]),
  ),
  [],
);
const rotation = object(
  {
    origin: vector,
    axis: { enum: ["x", "y", "z"] },
    angle: { enum: [-45, -22.5, 0, 22.5, 45] },
    rescale: { type: "boolean" },
  },
  ["origin", "axis", "angle"],
);
const id = { type: "string", pattern: "^[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}$" };
const element = object({ id, from: vector, to: vector, faces, rotation }, [
  "id",
  "from",
  "to",
  "faces",
]);
const palette = { type: "array", minItems: 1, maxItems: 256, items: color };
const pixelDocument = object({
  format: { const: "pcl-rh-pixel" },
  version: { const: 1 },
  width: integer(1, 128),
  height: integer(1, 128),
  palette,
  pixels: {
    type: "array",
    minItems: 1,
    maxItems: 16384,
    items: integer(0, 255),
  },
});
const modelDocument = object({
  format: { const: "pcl-rh-model" },
  version: { const: 1 },
  texture: pixelDocument,
  elements: { type: "array", maxItems: 128, items: element },
});
const pixelOperation = {
  oneOf: [
    object({ op: { const: "palette" }, index: integer(0, 255), color }),
    object({
      op: { const: "brush" },
      points: { type: "array", minItems: 1, maxItems: 16384, items: point },
      color: integer(0, 255),
    }),
    object(
      {
        op: { const: "rect" },
        x: integer(0, 127),
        y: integer(0, 127),
        width: integer(1, 128),
        height: integer(1, 128),
        color: integer(0, 255),
        filled: { type: "boolean" },
      },
      ["op", "x", "y", "width", "height", "color"],
    ),
    object({
      op: { const: "line" },
      from: point,
      to: point,
      color: integer(0, 255),
    }),
    object({
      op: { const: "fill" },
      x: integer(0, 127),
      y: integer(0, 127),
      color: integer(0, 255),
    }),
  ],
};
const pixelOperations = {
  type: "array",
  minItems: 1,
  maxItems: 256,
  items: pixelOperation,
};
const modelOperations = {
  type: "array",
  minItems: 1,
  maxItems: 256,
  items: {
    oneOf: [
      object({ op: { const: "add" }, element }),
      object({
        op: { const: "update" },
        id,
        patch: object(
          {
            from: vector,
            to: vector,
            faces,
            rotation: { anyOf: [rotation, { type: "null" }] },
          },
          [],
        ),
      }),
      object({ op: { const: "remove" }, id }),
      object({ op: { const: "texture" }, operations: pixelOperations }),
    ],
  },
};
const pathSchema = {
  type: "string",
  minLength: 1,
  maxLength: 512,
  description:
    "Visible captured project-relative .rhtexture.json or .rhmodel.json path; never an absolute path.",
};
const revision = integer(0, 128);
const schemas = {
  pixel: {
    create: object(
      {
        path: pathSchema,
        width: integer(1, 128),
        height: integer(1, 128),
        palette,
      },
      ["path"],
    ),
    edit: object({ path: pathSchema, revision, operations: pixelOperations }),
    inspect: object(
      {
        path: pathSchema,
        revision,
        region: object({
          x: integer(0, 127),
          y: integer(0, 127),
          width: integer(1, 32),
          height: integer(1, 32),
        }),
      },
      ["path"],
    ),
  },
  model: {
    create: object({ path: pathSchema, document: modelDocument }, ["path"]),
    edit: object({ path: pathSchema, revision, operations: modelOperations }),
    inspect: object(
      {
        path: pathSchema,
        revision,
        elementOffset: integer(0, 128),
        view: object(
          {
            yaw: {
              type: "number",
              minimum: -360,
              maximum: 360,
              description:
                "Horizontal camera orbit in degrees; default 45. Opposite view: add 180.",
            },
            pitch: {
              type: "number",
              minimum: -90,
              maximum: 90,
              description:
                "Camera elevation in degrees; default 30. Negative views show the underside.",
            },
          },
          [],
        ),
      },
      ["path"],
    ),
  },
};
const proposeSchema = object({
  path: pathSchema,
  revision: integer(1, 128),
  summary: { type: "string", minLength: 1, maxLength: 2000 },
});
const descriptions = {
  pixel: {
    create:
      "After harness.report_plan, create a private transparent pixel draft at a NEW .rhtexture.json source path. Defaults to 16×16, dimensions 1–128; optional RGBA palette starts with #00000000. Returns revision 1, palette/dimension summary and an actual deterministic PNG for immediate visual feedback. Use pixel.edit to draw; creation never writes the original project.",
    edit: "After harness.report_plan, atomically apply 1–256 structured pixel operations to the inspected revision. Revision 0 edits a frozen original; later revisions edit this job's private draft. Coordinates are integer [x,y], origin upper left. palette sets or appends an RGBA color at index; brush paints points, rect paints a filled/outline rectangle, line draws between endpoints, fill flood fills connected equal indices. Palette operations can precede drawing in the same batch. Returns the next revision and actual rendered PNG; failed batches do not change the draft.",
    inspect:
      "Read a .rhtexture.json from the frozen source (revision 0) or this job's draft. Omitted revision selects the latest draft or original. Returns actual PNG and structured dimensions, full palette and color usage; optional region returns rows of palette indices for a rectangle up to 32×32. Available in Ask, Plan and Execute without draft writes. Visually inspect after each edit before deciding the next operation.",
  },
  model: {
    create:
      "After harness.report_plan, create a private editable model at a NEW .rhmodel.json source path. Omit document for the default model or supply pcl-rh-model version 1 with embedded pcl-rh-pixel texture and up to 128 cuboid elements. Elements have id, from/to in -16..32, six optional faces with UV coordinates 0..16, optional axis rotation about an origin. Returns revision 1, structure summary and an actual PNG preview. Original project remains unchanged until separate user review.",
    edit: "After harness.report_plan, atomically apply 1–256 model edits to the inspected revision. Revision 0 starts from the frozen original. add inserts an element, update patches bounds/faces/rotation by id (rotation null removes it), remove deletes by id, texture applies structured pixel operations to the embedded texture. Cuboid bounds allow planes; UV coordinates 0..16 are independent of texture dimensions and may reverse. Returns next revision and actual deterministic PNG; inspect the image to verify shape, rotation and texture placement.",
    inspect:
      "Read a frozen or job-draft .rhmodel.json and return an actual deterministic PNG, texture summary and a page of eight complete elements. Omitted revision selects latest draft or revision 0 original. Continue elements with returned nextElementOffset as elementOffset. Optional view.yaw (-360..360) orbits the camera; view.pitch (-90..90) changes elevation, including rear/underside inspection. Defaults to yaw 45, pitch 30. Available in Ask, Plan and Execute and creates no draft. A rendered preview is visual feedback, not a game/runtime verification receipt.",
  },
};
function validate(kind, action, args, schema) {
  check(
    args &&
      typeof args === "object" &&
      !Array.isArray(args) &&
      Object.keys(args).every((key) => Object.hasOwn(schema.properties, key)) &&
      schema.required.every((key) => Object.hasOwn(args, key)),
    "INVALID_ARGUMENTS",
    "Expected declared visual tool fields",
  );
  sourcePath(args.path);
  check(
    args.path.endsWith(kind === "pixel" ? ".rhtexture.json" : ".rhmodel.json"),
    "INVALID_VISUAL_PATH",
    "Use the document extension belonging to this tool",
  );
  for (const [key, maximum] of [
    ["revision", 128],
    ["width", 128],
    ["height", 128],
    ["elementOffset", 128],
  ]) {
    if (args[key] !== undefined)
      check(
        Number.isInteger(args[key]) &&
          args[key] >= (["width", "height"].includes(key) ? 1 : 0) &&
          args[key] <= maximum,
        "INVALID_ARGUMENTS",
        `Invalid ${key}`,
      );
  }
  if (args.palette !== undefined)
    check(
      Array.isArray(args.palette) &&
        args.palette.length >= 1 &&
        args.palette.length <= 256 &&
        args.palette[0] === "#00000000" &&
        args.palette.every(
          (value) => typeof value === "string" && /^#[0-9A-F]{8}$/.test(value),
        ),
      "INVALID_ARGUMENTS",
      "Use up to 256 RGBA colors with transparent index zero",
    );
  if (args.document !== undefined) validateModel(args.document);
  if (args.operations !== undefined)
    check(
      Array.isArray(args.operations) &&
        args.operations.length >= 1 &&
        args.operations.length <= 256,
      "INVALID_ARGUMENTS",
      "Provide 1–256 structured operations",
    );
  if (args.region !== undefined)
    check(
      args.region &&
        Object.keys(args.region).sort().join() === "height,width,x,y" &&
        ["x", "y", "width", "height"].every(
          (key) =>
            Number.isInteger(args.region[key]) &&
            args.region[key] >= (["width", "height"].includes(key) ? 1 : 0) &&
            args.region[key] <= (["width", "height"].includes(key) ? 32 : 127),
        ),
      "INVALID_ARGUMENTS",
      "Use a bounded inspection rectangle",
    );
  if (args.view !== undefined)
    check(
      args.view &&
        typeof args.view === "object" &&
        !Array.isArray(args.view) &&
        Object.keys(args.view).every(
          (key) =>
            ["yaw", "pitch"].includes(key) &&
            Number.isFinite(args.view[key]) &&
            Math.abs(args.view[key]) <= (key === "yaw" ? 360 : 90),
        ),
      "INVALID_ARGUMENTS",
      "Use a bounded yaw/pitch camera view",
    );
  if (action === "propose")
    check(
      args.revision >= 1 &&
        typeof args.summary === "string" &&
        args.summary.trim() &&
        args.summary.length <= 2000,
      "INVALID_ARGUMENTS",
      "Provide a latest draft revision and concise review summary",
    );
  check(
    Buffer.byteLength(JSON.stringify(args)) <= 200_000,
    "INVALID_ARGUMENTS",
    "Visual input exceeds its byte budget",
  );
}
export function visualReceipt(result) {
  const { image, summary, ...rest } = result;
  return {
    ...rest,
    ...(image
      ? {
          image: {
            mimeType: image.mimeType,
            sha256: image.sha256,
            width: image.width,
            height: image.height,
            bytes: image.bytes,
          },
          contentStored: false,
        }
      : {}),
  };
}
export function registerVisualTools(registry, service, selectedKind) {
  check(
    service &&
      ["create", "edit", "inspect", "propose"].every(
        (name) => typeof service[name] === "function",
      ),
    "INVALID_VISUAL_HOST",
    "Supply the host-owned visual service",
  );
  check(
    selectedKind === undefined || ["pixel", "model"].includes(selectedKind),
    "INVALID_VISUAL_HOST",
    "Choose a visual tool owner",
  );
  for (const kind of selectedKind ? [selectedKind] : ["pixel", "model"])
    for (const action of ["create", "edit", "inspect", "propose"]) {
      const schema =
        action === "propose" ? proposeSchema : schemas[kind][action];
      registry.register({
        name: `${kind}.${action}`,
        owner: `rh.${kind}`,
        capability: `project.assets.${kind}`,
        modes: action === "inspect" ? ["ask", "plan", "execute"] : ["execute"],
        description:
          action === "propose"
            ? `After harness.report_plan, submit the latest ${kind} draft revision as a pending generic source-change review. Preserves the original frozen SHA-256, uses the visible JSON document path and never applies changes. The user separately approves and applies the proposal.`
            : descriptions[kind][action],
        inputSchema: schema,
        replaySafety: action === "inspect" ? "read-only" : "idempotent",
        ...(action !== "propose"
          ? { replayCompleted: true, projectReceipt: visualReceipt }
          : {}),
        validate: (args) => validate(kind, action, args, schema),
        execute(args, ctx) {
          throwIfAborted(ctx.signal);
          return service[action](ctx, kind, args);
        },
      });
    }
  return registry;
}
