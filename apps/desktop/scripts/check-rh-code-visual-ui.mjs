/** Actual visual view events and controller lifetimes against in-memory host
 * fixtures. No project directories, desktop, providers or credentials run. */
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync, existsSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import * as visuals from "../../../code/visual-assets.mjs";

const require = createRequire(import.meta.url),
  ts = require("typescript"),
  source = fileURLToPath(new URL("../src/rh-code/", import.meta.url));
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
function driver(file, exported, getProps, getController) {
  const modules = new Map(),
    hooks = [];
  let cursor = 0,
    dirty = false,
    effects = [],
    value,
    closed = false;
  const react = {
    createContext: (value) => ({ value }),
    useContext: (context) => context.value,
    useState(initial) {
      const index = cursor++;
      if (!(index in hooks))
        hooks[index] = typeof initial === "function" ? initial() : initial;
      return [
        hooks[index],
        (next) => {
          const result = typeof next === "function" ? next(hooks[index]) : next;
          if (!Object.is(result, hooks[index])) {
            hooks[index] = result;
            dirty = true;
          }
        },
      ];
    },
    useRef(initial) {
      return (hooks[cursor++] ??= { current: initial });
    },
    useMemo(compute, deps) {
      const index = cursor++,
        previous = hooks[index];
      if (
        !previous ||
        deps.some((item, index) => !Object.is(item, previous.deps[index]))
      )
        hooks[index] = { value: compute(), deps };
      return hooks[index].value;
    },
    useEffect(effect, deps) {
      const index = cursor++,
        previous = hooks[index];
      if (
        !previous ||
        deps.some((item, index) => !Object.is(item, previous.deps[index]))
      ) {
        hooks[index] = { deps };
        effects.push(() => {
          previous?.cleanup?.();
          hooks[index].cleanup = effect();
        });
      }
    },
  };
  function load(file) {
    if (modules.has(file)) return modules.get(file).exports;
    const module = { exports: {} };
    modules.set(file, module);
    const code = ts.transpileModule(readFileSync(file, "utf8"), {
      fileName: file,
      compilerOptions: {
        target: ts.ScriptTarget.ES2022,
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.ReactJSX,
      },
    }).outputText;
    function localRequire(id) {
      if (id === "react") return react;
      if (getController && id.endsWith("/controller"))
        return { useCode: getController };
      if (id.endsWith("/locale") || id === "./locale")
        return { label: (_, en) => en };
      if (id.endsWith("visual-assets.mjs")) return visuals;
      if (id.endsWith(".css")) return {};
      if (id.endsWith("CeSelect")) return { CeSelect: function CeSelect() {} };
      if (id.endsWith("experimentalTypes"))
        return {
          experimentalCall: (api, operation, args) =>
            api("experimental_call", { operation, args }),
        };
      if (id.startsWith(".")) {
        const base = path.resolve(path.dirname(file), id);
        return load(existsSync(base + ".tsx") ? base + ".tsx" : base + ".ts");
      }
      return require(id);
    }
    new Function("require", "module", "exports", code)(
      localRequire,
      module,
      module.exports,
    );
    return module.exports;
  }
  const component = load(path.join(source, file))[exported];
  function render() {
    if (closed) return;
    cursor = 0;
    dirty = false;
    effects = [];
    value = component(...getProps());
    for (const effect of effects) effect();
  }
  render();
  return {
    get value() {
      return value;
    },
    render,
    async flush() {
      for (let index = 0; index < 30; index++) {
        await new Promise((done) => setImmediate(done));
        if (closed || !dirty) return;
        render();
      }
      throw new Error("View did not settle");
    },
    close() {
      closed = true;
      for (const hook of hooks) hook?.cleanup?.();
    },
  };
}
function elements(node) {
  if (!node || typeof node !== "object") return [];
  if (Array.isArray(node)) return node.flatMap(elements);
  if (node.type?.name === "VectorInput")
    return [node, ...elements(node.type(node.props))];
  return [node, ...elements(node.props?.children)];
}
const element = (tree, label) => {
  const result = elements(tree).find(
    (node) => node.props?.["aria-label"] === label,
  );
  assert.ok(result, `Missing control: ${label}`);
  return result;
};
const descendants = (tree, name) =>
  elements(tree).filter((node) => node.type?.name === name);
function text(node) {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node !== "object") return String(node);
  if (Array.isArray(node)) return node.map(text).join("");
  return text(node.props?.children);
}
const project = (id) => ({
  id,
  name: id,
  folder: "/synthetic",
  updatedAt: 0,
  instructions: "",
  conversations: [],
  targets: [],
  layout: null,
});
function hostFixture() {
  const data = {
      revision: "r0",
      projects: [project("A"), project("B")],
      disabledPlugins: [],
      plugins: [],
      layoutPresets: [],
    },
    files = new Map(),
    calls = [],
    handlers = new Map();
  let revision = 0;
  const snapshot = () => structuredClone(data),
    commit = () => {
      data.revision = "r" + ++revision;
      return snapshot();
    };
  return {
    files,
    calls,
    handlers,
    snapshot,
    commit,
    async api(command, { operation, args = {} }) {
      calls.push({ command, operation, args: structuredClone(args) });
      if (operation === "bootstrap") return snapshot();
      if (operation === "status")
        return {
          jobs: [],
          liveConfigured: false,
          liveAvailable: false,
          aiPresets: { presets: [], selectedId: null },
        };
      if (handlers.has(operation)) return handlers.get(operation)(args);
      if (operation === "files")
        return {
          files: [...files.keys()]
            .filter((key) => key.startsWith(args.projectId + ":"))
            .map((key) => key.slice(args.projectId.length + 1)),
        };
      if (operation === "file_read") {
        const file = files.get(args.projectId + ":" + args.path);
        if (!file) throw new Error("File does not exist");
        return structuredClone(file);
      }
      assert.equal(args.expectedRevision, data.revision);
      if (operation === "asset_save" || operation === "file_save") {
        const key = args.projectId + ":" + args.path,
          previous = files.get(key);
        if (args.sha256 === null)
          assert.equal(
            previous,
            undefined,
            "New documents cannot overwrite an existing file",
          );
        else
          assert.equal(args.sha256, previous?.sha256, "Disk document changed");
        files.set(key, { content: args.content, sha256: "hash-" + revision });
      } else if (operation === "layout_save") {
        data.projects.find((value) => value.id === args.projectId).layout =
          args.layout;
      } else if (operation !== "asset_export")
        throw new Error("Unexpected fixture operation " + operation);
      return commit();
    },
  };
}
async function fixture(t) {
  const host = hostFixture(),
    controller = driver("controller.tsx", "useController", () => [
      host.api,
      true,
    ]);
  t.after(() => controller.close());
  await controller.flush();
  controller.value.choose(controller.value.snapshot.projects[0]);
  await controller.flush();
  const views = [];
  return {
    host,
    controller,
    view(file, exported, props) {
      const view = driver(
        "visual-assets/" + file,
        exported,
        () => [props],
        () => controller.value,
      );
      views.push(view);
      t.after(() => view.close());
      return view;
    },
    async flush() {
      await controller.flush();
      for (const view of views) {
        view.render();
        await view.flush();
      }
      await controller.flush();
    },
  };
}
const areaProps = (view) => ({
  area: { type: "area", id: "fixture-area", view },
  api: {
    state: {},
    updateState(next) {
      this.state = next;
    },
    keepAwake: () => () => {},
    invoke() {},
  },
});
const serialize = (document) => JSON.stringify(document, null, 2) + "\n";

test("new controller-owned asset drafts survive area closure and A → B → A, then create-only save", async (t) => {
  const fixture = await fixtureFor(t),
    { controller, host } = fixture;
  controller.value.createAssetDocument(
    "textures/new.rhtexture.json",
    serialize(visuals.createPixel()),
  );
  await fixture.flush();
  assert.equal(controller.value.getLifecycleState().dirty, true);
  controller.value.choose(controller.value.snapshot.projects[1]);
  await fixture.flush();
  assert.deepEqual(controller.value.assetDocumentPaths("pixel"), []);
  controller.value.choose(controller.value.snapshot.projects[0]);
  await fixture.flush();
  const props = areaProps("pixel");
  const hook = driver(
    "visual-assets/useAssetDocument.ts",
    "useAssetDocument",
    () => ["pixel", props],
    () => controller.value,
  );
  t.after(() => hook.close());
  await hook.flush();
  assert.ok(hook.value.files.includes("textures/new.rhtexture.json"));
  await controller.value.saveFile("textures/new.rhtexture.json");
  await fixture.flush();
  const call = host.calls.find((call) => call.operation === "asset_save");
  assert.equal(call.args.sha256, null);
  assert.equal(call.args.projectId, "A");
  assert.equal(
    controller.value.buffer("textures/new.rhtexture.json").content,
    host.files.get("A:textures/new.rhtexture.json").content,
  );
});
// Avoid shadowing the fixture constructor with the scoped fixture variables.
const fixtureFor = fixture;

test("save acknowledgement preserves a newer asset edit and routes existing documents with their hash", async (t) => {
  const fixture = await fixtureFor(t),
    { controller, host } = fixture,
    file = "models/example.rhmodel.json",
    document = visuals.createModel();
  controller.value.createAssetDocument(file, serialize(document));
  await fixture.flush();
  const waiting = deferred();
  host.handlers.set("asset_save", async (args) => {
    assert.equal(args.sha256, null);
    await waiting.promise;
    host.files.set("A:" + file, {
      content: args.content,
      sha256: "created-hash",
    });
    return host.commit();
  });
  const saving = controller.value.saveFile(file);
  await fixture.flush();
  const newer = serialize(
    visuals.editModel(document, [
      { op: "update", id: "cube", patch: { to: [12, 16, 16] } },
    ]),
  );
  controller.value.edit(file, newer);
  waiting.resolve();
  await saving;
  await fixture.flush();
  assert.equal(controller.value.buffer(file).content, newer);
  assert.equal(controller.value.buffer(file).original, serialize(document));
  host.handlers.delete("asset_save");
  await controller.value.saveFile(file);
  await fixture.flush();
  assert.equal(
    host.calls.filter((call) => call.operation === "asset_save").at(-1).args
      .sha256,
    "created-hash",
  );
  assert.equal(host.files.get("A:" + file).content, newer);
});

test("asset save collision retains the draft, and suspend drains assets even when their view is closed", async (t) => {
  const fixture = await fixtureFor(t),
    { controller, host } = fixture,
    file = "texture.rhtexture.json",
    content = serialize(visuals.createPixel());
  controller.value.createAssetDocument(file, content);
  await fixture.flush();
  host.files.set("A:" + file, { content: "external", sha256: "external-hash" });
  await assert.rejects(controller.value.saveFile(file), /cannot overwrite/);
  await fixture.flush();
  assert.equal(controller.value.buffer(file).content, content);
  assert.equal(controller.value.buffer(file).sha256, null);
  assert.equal(await controller.value.prepareSuspend(), false);
  host.files.delete("A:" + file);
  assert.equal(await controller.value.prepareSuspend(), true);
  assert.equal(host.files.get("A:" + file).content, content);
});

test("pixel controls use the latest source draft, and one undo reverses a complete pointer stroke", async (t) => {
  const fixture = await fixtureFor(t),
    props = areaProps("pixel"),
    file = "pixel.rhtexture.json",
    document = visuals.editPixel(visuals.createPixel(), [
      { op: "palette", index: 1, color: "#AABBCCFF" },
    ]);
  fixture.controller.value.createAssetDocument(file, serialize(document));
  props.api.state = { path: file };
  await fixture.flush();
  const view = fixture.view("PixelEditor.tsx", "PixelEditor", props);
  await fixture.flush();
  const texture = descendants(view.value, "TexturePanel")[0];
  assert.ok(texture);
  // Continuous pointer events can arrive before React redraws the view.
  texture.props.edit([{ op: "brush", points: [[0, 0]], color: 1 }], true);
  texture.props.edit(
    [{ op: "line", from: [0, 0], to: [3, 0], color: 1 }],
    false,
  );
  texture.props.edit(
    [{ op: "line", from: [3, 0], to: [3, 3], color: 1 }],
    false,
  );
  await fixture.flush();
  const painted = JSON.parse(fixture.controller.value.buffer(file).content);
  assert.equal(painted.pixels[0], 1);
  assert.equal(painted.pixels[3 * 16 + 3], 1);
  const toolbar = descendants(view.value, "AssetToolbar")[0];
  toolbar.props.session.undo();
  await fixture.flush();
  assert.deepEqual(
    JSON.parse(fixture.controller.value.buffer(file).content),
    visuals.validatePixel(document),
  );
  descendants(view.value, "AssetToolbar")[0].props.session.redo();
  await fixture.flush();
  assert.equal(
    JSON.parse(fixture.controller.value.buffer(file).content).pixels[51],
    1,
  );
  const newer = visuals.editPixel(painted, [
    { op: "brush", points: [[15, 15]], color: 1 },
  ]);
  fixture.controller.value.edit(file, serialize(newer));
  toolbar.props.session.undo();
  assert.equal(fixture.controller.value.buffer(file).content, serialize(newer));
  // The earlier canvas callback must preserve that source-editor change.
  texture.props.edit([{ op: "brush", points: [[1, 1]], color: 1 }], true);
  assert.equal(
    JSON.parse(fixture.controller.value.buffer(file).content).pixels[255],
    1,
  );
});

test("brush stroke that starts on an unchanged pixel records undo at its first effective edit", () => {
  const pixel = visuals.editPixel(visuals.createPixel(), [
    { op: "palette", index: 1, color: "#FFFFFFFF" },
    { op: "brush", points: [[0, 0]], color: 1 },
  ]);
  const records = [],
    target = {
      setPointerCapture() {},
      hasPointerCapture() {
        return true;
      },
      releasePointerCapture() {},
      getBoundingClientRect() {
        return { left: 0, top: 0, width: 160, height: 160 };
      },
    };
  let current = pixel;
  const view = driver("visual-assets/TexturePanel.tsx", "TexturePanel", () => [
    {
      document: current,
      resize() {},
      edit(operations, record) {
        const next = visuals.editPixel(current, operations),
          changed = JSON.stringify(next) !== JSON.stringify(current);
        if (changed) {
          records.push(record);
          current = next;
        }
        return changed;
      },
    },
  ]);
  const canvas = elements(view.value).find((node) => node.type === "canvas"),
    pointer = (x, y) => ({
      button: 0,
      pointerId: 1,
      clientX: x,
      clientY: y,
      currentTarget: target,
      preventDefault() {},
    });
  canvas.props.onPointerDown(pointer(2, 2));
  canvas.props.onPointerMove(pointer(12, 2));
  canvas.props.onPointerMove(pointer(22, 2));
  canvas.props.onPointerUp(pointer(22, 2));
  assert.deepEqual(records, [true, false]);
  view.close();
});

test("model geometry exposes independent face UVs, official rotation angles and valid cuboid operations", async (t) => {
  let document = visuals.createModel(),
    selected = "cube";
  const operations = [],
    view = driver("visual-assets/ModelGeometry.tsx", "ModelGeometry", () => [
      {
        document,
        selected,
        select(id) {
          selected = id;
        },
        edit(next) {
          document = visuals.editModel(document, next);
          operations.push(next);
        },
      },
    ]);
  t.after(() => view.close());
  element(view.value, "From X").props.onChange({ target: { value: "2" } });
  view.render();
  assert.equal(document.elements[0].from[0], 2);
  element(view.value, "North U1").props.onChange({ target: { value: "3" } });
  view.render();
  assert.equal(document.elements[0].faces.north.uv[0], 3);
  assert.equal(document.elements[0].faces.south.uv[0], 0);
  element(view.value, "Add cuboid").props.onClick();
  view.render();
  assert.equal(document.elements.length, 2);
  assert.equal(selected, "cube-1");
  const checkbox = elements(view.value).find(
    (node) =>
      node.type === "input" &&
      node.props.type === "checkbox" &&
      !node.props["aria-label"],
  );
  checkbox.props.onChange({ target: { checked: true } });
  view.render();
  element(view.value, "Element rotation angle").props.onChange({
    target: { value: "22.5" },
  });
  view.render();
  assert.equal(document.elements[1].rotation.angle, 22.5);
  element(view.value, "Remove selected cuboid").props.onClick();
  view.render();
  assert.equal(document.elements.length, 1);
  assert.deepEqual(operations.at(-1), [{ op: "remove", id: "cube-1" }]);
});

test("model export sends paired paths, exact draft and texture reference through one revisioned write", async (t) => {
  const fixture = await fixtureFor(t),
    file = "export.rhmodel.json";
  fixture.controller.value.createAssetDocument(
    file,
    serialize(visuals.createModel()),
  );
  await fixture.flush();
  const session = {
      c: fixture.controller.value,
      document: JSON.parse(fixture.controller.value.buffer(file).content),
      pending: false,
      async work(action) {
        await action();
      },
    },
    view = fixture.view("AssetToolbar.tsx", "AssetExport", {
      kind: "model",
      session,
    });
  element(view.value, "Export path").props.onChange({
    target: { value: "src/main/resources/assets/demo/models/item/test.json" },
  });
  element(view.value, "Texture PNG output path").props.onChange({
    target: { value: "src/main/resources/assets/demo/textures/item/test.png" },
  });
  element(view.value, "Texture resource name").props.onChange({
    target: { value: "demo:item/test" },
  });
  view.render();
  const button = elements(view.value).find((node) => node.type === "button");
  await button.props.onClick();
  await fixture.flush();
  const exports = fixture.host.calls.filter(
    (call) => call.operation === "asset_export",
  );
  assert.equal(exports.length, 1);
  assert.equal(
    exports[0].args.texturePath,
    "src/main/resources/assets/demo/textures/item/test.png",
  );
  assert.equal(exports[0].args.textureResource, "demo:item/test");
  assert.equal(
    exports[0].args.path,
    "src/main/resources/assets/demo/models/item/test.json",
  );
  assert.deepEqual(JSON.parse(exports[0].args.content), session.document);
  assert.equal(exports[0].args.expectedRevision, "r0");
});

test("AI visual receipts display successive revisions and import a copy without overwriting an owned draft", async (t) => {
  const fixture = await fixtureFor(t),
    props = areaProps("pixel"),
    file = "ai-copy.rhtexture.json",
    document = visuals.editPixel(visuals.createPixel(), [
      { op: "palette", index: 1, color: "#336699FF" },
      { op: "fill", x: 0, y: 0, color: 1 },
    ]);
  fixture.controller.value.project.conversations.push({
    id: "thread",
    activeJob: "visual-job",
    messages: [],
  });
  const receipt = {
    summary: {
      jobId: "visual-job",
      revision: 1,
      status: "running",
      updatedAt: "2026-10-13T00:00:00Z",
    },
    artifacts: [],
    reviews: [],
    visualArtifacts: [
      {
        kind: "pixel",
        path: "generated.rhtexture.json",
        revision: 1,
        document,
        image: {
          mimeType: "image/png",
          data: "preview-base64-1",
          width: 16,
          height: 16,
        },
      },
    ],
  };
  fixture.controller.value.jobs["visual-job"] = receipt;
  const hook = driver(
    "visual-assets/useAssetDocument.ts",
    "useAssetDocument",
    () => ["pixel", props],
    () => fixture.controller.value,
  );
  t.after(() => hook.close());
  await hook.flush();
  const previewProps = { kind: "pixel", session: hook.value },
    view = fixture.view("AiAssetPreview.tsx", "AiAssetPreview", previewProps);
  await fixture.flush();
  assert.equal(
    elements(view.value).find((node) => node.type === "img").props.src,
    "data:image/png;base64,preview-base64-1",
  );
  receipt.visualArtifacts[0] = {
    ...receipt.visualArtifacts[0],
    revision: 2,
    image: { ...receipt.visualArtifacts[0].image, data: "preview-base64-2" },
  };
  view.render();
  assert.ok(text(view.value).includes("r2"));
  assert.equal(
    elements(view.value).find((node) => node.type === "img").props.src,
    "data:image/png;base64,preview-base64-2",
  );
  element(view.value, "AI draft import name").props.onChange({
    target: { value: "ai-copy" },
  });
  view.render();
  const button = elements(view.value).find((node) => node.type === "button");
  button.props.onClick();
  await fixture.flush();
  hook.render();
  assert.equal(props.api.state.path, file);
  assert.deepEqual(
    JSON.parse(fixture.controller.value.buffer(file).content),
    document,
  );
  fixture.controller.value.edit(
    file,
    serialize(
      visuals.editPixel(document, [
        { op: "brush", points: [[0, 0]], color: 0 },
      ]),
    ),
  );
  button.props.onClick();
  await fixture.flush();
  hook.render();
  assert.equal(
    JSON.parse(fixture.controller.value.buffer(file).content).pixels[0],
    0,
  );
  assert.ok(hook.value.error.includes("open buffer"));
  delete receipt.visualArtifacts[0].document;
  view.render();
  assert.equal(
    elements(view.value).find((node) => node.type === "button").props.disabled,
    true,
  );
  assert.ok(text(view.value).includes("preview omits the document"));
});

test("project AI preview selection covers parallel conversations, follows a stable job/path and surfaces preview failures", async (t) => {
  const fixture = await fixtureFor(t),
    props = areaProps("pixel"),
    document = visuals.createPixel(),
    artifact = (path, revision, data) => ({
      kind: "pixel",
      path,
      revision,
      document,
      image: { mimeType: "image/png", data, width: 16, height: 16 },
    }),
    receipt = (jobId, artifacts, visualError) => ({
      summary: { jobId, revision: 1, status: "running" },
      artifacts: [],
      reviews: [],
      visualArtifacts: artifacts,
      ...(visualError ? { visualError } : {}),
    });
  const one = receipt("job-one", [
      artifact("shared.rhtexture.json", 1, "one-shared"),
      artifact("variant.rhtexture.json", 1, "one-variant"),
    ]),
    two = receipt("job-two", [
      artifact("shared.rhtexture.json", 1, "two-shared"),
    ]),
    broken = receipt(
      "job-error",
      [],
      "Visual preview is unavailable; inspect the task.",
    ),
    other = receipt("job-other-project", [
      artifact("variant.rhtexture.json", 1, "other-project"),
    ]);
  fixture.controller.value.project.conversations.push(
    { id: "one", title: "Texture studies", activeJob: "job-one", messages: [] },
    {
      id: "two",
      title: "Color variations",
      activeJob: "job-two",
      messages: [],
    },
    {
      id: "broken",
      title: "Unavailable draft",
      activeJob: "job-error",
      messages: [],
    },
  );
  fixture.controller.value.snapshot.projects[1].conversations.push({
    id: "other",
    title: "Other project",
    activeJob: "job-other-project",
    messages: [],
  });
  Object.assign(fixture.controller.value.jobs, {
    "job-one": one,
    "job-two": two,
    "job-error": broken,
    "job-other-project": other,
  });
  const hook = driver(
    "visual-assets/useAssetDocument.ts",
    "useAssetDocument",
    () => ["pixel", props],
    () => fixture.controller.value,
  );
  t.after(() => hook.close());
  await hook.flush();
  const previewProps = { kind: "pixel", session: hook.value },
    view = fixture.view("AiAssetPreview.tsx", "AiAssetPreview", previewProps);
  await fixture.flush();
  const choices = elements(view.value).filter((node) => node.type === "option");
  assert.equal(choices.length, 3);
  assert.ok(text(choices).includes("variant.rhtexture.json · Texture studies"));
  assert.ok(text(choices).includes("shared.rhtexture.json · Color variations"));
  assert.ok(!text(choices).includes("Other project"));
  assert.equal(
    elements(view.value).find((node) => node.type === "img").props.src,
    "data:image/png;base64,two-shared",
  );
  assert.ok(
    text(
      elements(view.value).find((node) => node.props?.role === "alert"),
    ).includes("Unavailable draft"),
  );
  const key = JSON.stringify(["job-one", "variant.rhtexture.json"]);
  element(view.value, "Choose AI asset preview").props.onChange({
    target: { value: key },
  });
  view.render();
  element(view.value, "AI draft import name").props.onChange({
    target: { value: "private-A-name" },
  });
  view.render();
  // Reordering bounded receipts and receiving a new revision do not select a
  // different task or erase the copy name being typed for the selected path.
  one.visualArtifacts = [
    artifact("variant.rhtexture.json", 4, "one-variant-r4"),
    one.visualArtifacts[0],
  ];
  one.summary.revision = 2;
  view.render();
  assert.equal(element(view.value, "Choose AI asset preview").props.value, key);
  assert.equal(
    elements(view.value).find((node) => node.type === "img").props.src,
    "data:image/png;base64,one-variant-r4",
  );
  assert.equal(
    element(view.value, "AI draft import name").props.value,
    "private-A-name",
  );
  assert.ok(text(view.value).includes("r4"));
  one.visualArtifacts = [];
  two.visualArtifacts = [];
  view.render();
  assert.equal(
    elements(view.value).filter((node) => node.type === "img").length,
    0,
  );
  assert.ok(text(view.value).includes("Visual preview is unavailable"));
  fixture.controller.value.choose(
    fixture.controller.value.snapshot.projects[1],
  );
  await fixture.controller.flush();
  hook.render();
  await hook.flush();
  previewProps.session = hook.value;
  view.render();
  await view.flush();
  assert.equal(
    elements(view.value).find((node) => node.type === "img").props.src,
    "data:image/png;base64,other-project",
  );
  assert.equal(element(view.value, "AI draft import name").props.value, "");
  assert.ok(!text(view.value).includes("Unavailable draft"));
  elements(view.value)
    .find((node) => node.type === "button")
    .props.onClick();
  await fixture.controller.flush();
  assert.ok(
    fixture.controller.value
      .assetDocumentPaths("pixel")
      .includes("variant-copy.rhtexture.json"),
  );
  assert.ok(
    !fixture.controller.value
      .assetDocumentPaths("pixel")
      .includes("private-A-name.rhtexture.json"),
  );
});

test("texture import ignores an old project reply and preserves model edits made during the read", async (t) => {
  const fixture = await fixtureFor(t),
    props = areaProps("model"),
    file = "model.rhmodel.json",
    original = visuals.createModel();
  fixture.controller.value.createAssetDocument(file, serialize(original));
  props.api.state = { path: file };
  await fixture.flush();
  const waiting = deferred();
  fixture.host.files.set("A:texture.rhtexture.json", {
    content: serialize(visuals.createPixel(32, 32)),
    sha256: "texture-hash",
  });
  fixture.host.files.set("A:late.rhtexture.json", {
    content: serialize(visuals.createPixel(64, 64)),
    sha256: "late-hash",
  });
  fixture.host.handlers.set("file_read", (args) =>
    args.path === "texture.rhtexture.json"
      ? waiting.promise
      : structuredClone(
          fixture.host.files.get(args.projectId + ":" + args.path),
        ),
  );
  const view = fixture.view("ModelEditor.tsx", "ModelEditor", props);
  await fixture.flush();
  element(view.value, "Import project texture document").props.onChange({
    target: { value: "texture.rhtexture.json" },
  });
  view.render();
  const importButton = elements(view.value).find(
    (node) => node.type === "button" && text(node) === "Import",
  );
  assert.ok(importButton);
  importButton.props.onClick();
  await fixture.flush();
  const edited = serialize(
    visuals.editModel(original, [
      { op: "update", id: "cube", patch: { to: [12, 16, 16] } },
    ]),
  );
  fixture.controller.value.edit(file, edited);
  waiting.resolve({
    content: serialize(visuals.createPixel(32, 32)),
    sha256: "texture-hash",
  });
  await fixture.flush();
  assert.equal(fixture.controller.value.buffer(file).content, edited);
  assert.ok(
    descendants(view.value, "AssetToolbar")[0].props.session.error.includes(
      "The model changed",
    ),
  );
  // A completed texture read is now cached; use another delayed source to test navigation.
  const late = deferred();
  fixture.host.handlers.set("file_read", () => late.promise);
  element(view.value, "Import project texture document").props.onChange({
    target: { value: "late.rhtexture.json" },
  });
  view.render();
  elements(view.value)
    .find((node) => node.type === "button" && text(node) === "Import")
    .props.onClick();
  await fixture.flush();
  fixture.controller.value.choose(
    fixture.controller.value.snapshot.projects[1],
  );
  await fixture.flush();
  late.resolve({
    content: serialize(visuals.createPixel(64, 64)),
    sha256: "late-hash",
  });
  await fixture.flush();
  fixture.controller.value.choose(
    fixture.controller.value.snapshot.projects[0],
  );
  await fixture.flush();
  assert.equal(fixture.controller.value.buffer(file).content, edited);
});
