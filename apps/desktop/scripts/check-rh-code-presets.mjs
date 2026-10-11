/** Exercise actual preset helpers against synthetic JSON only. No project
 * store, conversation messages, launcher data or native host is accessed. */
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const esbuild = require("esbuild");
const dir = await mkdtemp(path.join(tmpdir(), "rh-code-presets-"));
try {
  await esbuild.build({
    entryPoints: [
      new URL("../src/rh-code/layoutPresets.ts", import.meta.url).pathname,
    ],
    bundle: true,
    platform: "node",
    format: "esm",
    outfile: path.join(dir, "presets.mjs"),
  });
  const helpers = await import(pathToFileURL(path.join(dir, "presets.mjs")));
  let serial = 0;
  const area = (view = "chat", fields = {}) => ({
    type: "area",
    id: `area-${++serial}`,
    view,
    ...fields,
  });
  const split = (a, b, fields = {}) => ({
    type: "split",
    id: `split-${++serial}`,
    axis: "row",
    ratio: 0.5,
    children: [a, b],
    ...fields,
  });
  const nodes = (tree) =>
    tree.type === "area" ? [tree] : [tree, ...tree.children.flatMap(nodes)];
  const encodeRaw = (tree, fields = {}) =>
    JSON.stringify({
      format: "pcl-rh-code-layout",
      version: 1,
      name: "Fixture",
      layout: tree,
      ...fields,
    });

  const privateTree = split(
    area("chat", {
      conversation: "private-one",
      state: { chat: { draft: "private draft", options: [true, null, 2] } },
    }),
    split(
      area("vendor.unknown-view", {
        file: "src/Main.java",
        state: { "vendor.unknown-view": { cursor: 10 } },
      }),
      area("chat", { conversation: "private-two" }),
      { axis: "column", ratio: 0.85 },
    ),
    { ratio: 0.15 },
  );
  const before = structuredClone(privateTree);
  const portable = helpers.portableLayout(privateTree);
  assert.deepEqual(privateTree, before, "Export must not mutate the live tree");
  assert.deepEqual(
    nodes(portable).map((node) => node.id),
    nodes(privateTree).map((node) => node.id),
    "Export preserves stable node identity",
  );
  assert.equal(portable.children[1].children[0].view, "vendor.unknown-view");
  for (const node of nodes(portable)) {
    assert.equal(Object.hasOwn(node, "file"), false);
    assert.equal(Object.hasOwn(node, "state"), false);
    assert.equal(Object.hasOwn(node, "conversation"), false);
  }
  const text = helpers.encodeLayoutPreset("  My layout  ", privateTree);
  assert.match(text, /\n  "format"/);
  assert.equal(text.includes("private"), false);
  assert.deepEqual(helpers.decodeLayoutPreset(text), {
    name: "My layout",
    layout: portable,
  });
  assert.deepEqual(
    helpers.decodeLayoutPreset(encodeRaw(privateTree)).layout,
    portable,
  );
  assert.equal(
    helpers.decodeLayoutPreset(encodeRaw(area(), { name: "😀".repeat(80) }))
      .name.length,
    160,
  );

  const applied = helpers.instantiateLayout(privateTree, [
    "new-one",
    "new-two",
  ]);
  const oldIds = new Set(nodes(privateTree).map((node) => node.id));
  const freshIds = nodes(applied).map((node) => node.id);
  assert.equal(new Set(freshIds).size, freshIds.length);
  assert.equal(
    freshIds.some((id) => oldIds.has(id)),
    false,
  );
  assert.deepEqual(
    nodes(applied)
      .filter((node) => node.view === "chat")
      .map((node) => node.conversation),
    ["new-one", "new-two"],
  );
  assert.equal(JSON.stringify(applied).includes("private"), false);
  assert.equal(JSON.stringify(applied).includes("Main.java"), false);
  const unbound = helpers.instantiateLayout(privateTree, ["only-one"]);
  assert.equal(
    Object.hasOwn(unbound.children[1].children[1], "conversation"),
    false,
  );
  assert.throws(
    () => helpers.instantiateLayout(privateTree, ["bad ID"]),
    /conversation/,
  );

  for (const malformed of [
    null,
    area("bad view"),
    area("_starts-invalid"),
    area("v".repeat(121)),
    area("chat", { id: "x".repeat(101) }),
    area("chat", { type: "unknown" }),
    area("chat", { extra: true }),
    area("chat", { conversation: null }),
    split(area(), area(), { ratio: NaN }),
    split(area(), area(), { ratio: 0.14 }),
    split(area(), area(), { ratio: 0.86 }),
    split(area(), area(), { axis: "vertical" }),
    split(area(), area(), { children: [area()] }),
    split(area(), area(), { children: [area(), area(), area()] }),
  ])
    assert.throws(() => helpers.portableLayout(malformed));
  const duplicate = split(area(), area());
  duplicate.children[1].id = duplicate.id;
  assert.throws(() => helpers.portableLayout(duplicate), /Duplicate/);
  for (const file of [
    "",
    " ",
    "/absolute",
    "a\\b",
    "a:b",
    "a//b",
    "../a",
    "a/.secret",
    "a\nb",
    "x".repeat(4097),
  ])
    assert.throws(
      () => helpers.portableLayout(area("source", { file })),
      /file/,
    );

  let deep = area();
  for (let index = 0; index < 7; index++) deep = split(deep, area());
  helpers.portableLayout(deep);
  assert.throws(() => helpers.portableLayout(split(deep, area())), /deep/);
  const balanced = (count) =>
    count === 1
      ? area()
      : split(balanced(Math.floor(count / 2)), balanced(Math.ceil(count / 2)));
  helpers.portableLayout(balanced(12));
  assert.throws(() => helpers.portableLayout(balanced(13)), /12 areas/);

  let state = { text: "value" };
  for (let index = 0; index < 6; index++) state = { nested: state };
  helpers.portableLayout(area("chat", { state: { chat: state } }));
  state = { nested: { nested: state } };
  assert.throws(
    () => helpers.portableLayout(area("chat", { state: { chat: state } })),
    /deep/,
  );
  for (const state of [
    [],
    { chat: [] },
    { chat: { invalid: undefined } },
    { chat: { number: Infinity } },
    { chat: { "bad key": true } },
  ])
    assert.throws(() => helpers.portableLayout(area("chat", { state })));
  assert.throws(
    () =>
      helpers.portableLayout(
        area("chat", { state: { chat: { text: "汉".repeat(22000) } } }),
      ),
    /64 KiB/,
  );
  const large = balanced(5);
  for (const node of nodes(large).filter((node) => node.type === "area"))
    node.state = { chat: { text: "x".repeat(60000) } };
  assert.throws(() => helpers.portableLayout(large), /256 KiB/);

  for (const key of ["__proto__", "constructor", "prototype"]) {
    const unsafeState = JSON.parse(`{"chat":{"${key}":{}}}`);
    assert.throws(
      () => helpers.portableLayout(area("chat", { state: unsafeState })),
      /Unsafe/,
    );
    const unsafeNode = JSON.parse(
      `{"type":"area","id":"safe","view":"chat","${key}":{}}`,
    );
    assert.throws(() => helpers.portableLayout(unsafeNode), /Unsafe/);
  }
  const getter = area();
  Object.defineProperty(getter, "view", {
    enumerable: true,
    get() {
      throw new Error("must not execute");
    },
  });
  assert.throws(
    () => helpers.portableLayout(getter),
    /Unsupported JSON property/,
  );
  for (const input of ["{", "[]", "null", " ".repeat(262145)])
    assert.throws(() => helpers.decodeLayoutPreset(input));
  for (const fields of [
    { version: 2 },
    { version: "1" },
    { format: "other" },
    { name: " " },
    { name: "x".repeat(81) },
    { name: "line\nbreak" },
    { extra: "unsupported" },
    { layout: null },
  ])
    assert.throws(() => helpers.decodeLayoutPreset(encodeRaw(area(), fields)));
  assert.throws(
    () =>
      helpers.decodeLayoutPreset(encodeRaw(area(), { name: "😀".repeat(81) })),
    /name/,
  );
  console.log(
    "RH Code presets: portable roundtrip, private-data stripping, fresh identities, sequential chat binding and bounded hostile-input validation passed.",
  );
} finally {
  await rm(dir, { recursive: true, force: true });
}
