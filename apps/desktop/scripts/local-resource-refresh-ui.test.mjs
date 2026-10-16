/** Resource discovery uses the actual list, update hooks and instance wrapper.
 * All inventory, task generations and native replies are synthetic in memory. */
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { content, deferred, elements } from "./maker-test-harness.mjs";

const require = createRequire(import.meta.url),
  ts = require("typescript"),
  source = fileURLToPath(new URL("../src/", import.meta.url));

function resourceHarness(initial, navigation, wrapper = false) {
  const hooks = [],
    modules = new Map();
  let props = initial,
    cursor = 0,
    dirty = false,
    pending = [],
    tree,
    closed = false;
  const react = {
    useState(initial) {
      const index = cursor++;
      if (!(index in hooks))
        hooks[index] = typeof initial === "function" ? initial() : initial;
      return [
        hooks[index],
        (next) => {
          const value = typeof next === "function" ? next(hooks[index]) : next;
          if (!Object.is(value, hooks[index])) {
            hooks[index] = value;
            dirty = true;
          }
        },
      ];
    },
    useRef(value) {
      return (hooks[cursor++] ??= { current: value });
    },
    useContext(context) {
      return context.value;
    },
    useEffect(effect, dependencies) {
      const index = cursor++,
        previous = hooks[index];
      if (
        !previous ||
        dependencies.some(
          (value, i) => !Object.is(value, previous.dependencies[i]),
        )
      ) {
        hooks[index] = { dependencies };
        pending.push(() => {
          previous?.cleanup?.();
          hooks[index].cleanup = effect();
        });
      }
    },
  };
  const placeholders = {
    "./CeSelect": { CeSelect() {} },
    "./Collapse": { Collapse() {} },
    "./InstanceOperations": { InstanceOperations() {} },
    "./JavaSelect": { JavaSelect() {} },
    "./InstanceTrash": { InstanceDelete() {} },
    "./LauncherFavorites": { ResourceFavorite() {} },
    "./useLauncherPreferences": {
      LauncherNavigationContext: { value: navigation },
    },
  };
  function load(file) {
    if (modules.has(file)) return modules.get(file).exports;
    const module = { exports: {} };
    modules.set(file, module);
    const code = ts.transpileModule(fs.readFileSync(file, "utf8"), {
      fileName: file,
      compilerOptions: {
        target: ts.ScriptTarget.ES2022,
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.ReactJSX,
      },
    }).outputText;
    function localRequire(id) {
      if (id === "react") return react;
      if (placeholders[id]) return placeholders[id];
      if (id.endsWith(".css")) return {};
      if (id.endsWith(".png")) return id;
      if (id.startsWith(".")) {
        const base = path.resolve(path.dirname(file), id);
        return load(
          fs.existsSync(`${base}.tsx`) ? `${base}.tsx` : `${base}.ts`,
        );
      }
      return require(id);
    }
    new Function("require", "module", "exports", "window", "document", code)(
      localRequire,
      module,
      module.exports,
      { addEventListener() {}, removeEventListener() {} },
      { activeElement: null },
    );
    return module.exports;
  }
  const component = load(
      path.join(source, wrapper ? "InstancesPanel.tsx" : "LocalResources.tsx"),
    ),
    i18n = load(path.join(source, "i18n.ts"));
  function render() {
    cursor = 0;
    dirty = false;
    pending = [];
    if (wrapper) {
      const panel = component.InstancePanel(props);
      tree = panel.type(panel.props);
    } else tree = component.ResourcePanel(props);
    for (const effect of pending) effect();
  }
  render();
  return {
    i18n,
    get tree() {
      return tree;
    },
    replace(next) {
      props = { ...props, ...next };
      render();
    },
    async flush() {
      for (let n = 0; n < 50; n++) {
        await new Promise((done) => setImmediate(done));
        if (!dirty || closed) return;
        render();
      }
      throw Error("Resource component did not settle");
    },
    close() {
      closed = true;
      for (const hook of hooks) hook?.cleanup?.();
    },
  };
}

function resource(name, kind = "mods") {
  const file = `${name}.${kind === "mods" ? "jar" : "zip"}`;
  return {
    name,
    path: `/synthetic/root-a/${kind}/${file}`,
    file_name: file,
    fingerprint: `${name}-fingerprint`,
    enabled: true,
  };
}
function fixture(kind = "mods", initial = [], options = {}) {
  const calls = [],
    browsed = [];
  let inventory = initial,
    removed = [],
    readOverride = null;
  const instance = {
    id: "sample",
    minecraft_version: "1.21.1",
    loader: "NeoForge 21.1.200",
  };
  const api = async (command, args) => {
    calls.push({ command, args });
    if (command === "instance_resources")
      return readOverride ? readOverride() : structuredClone(inventory);
    if (command === "resource_removed") return structuredClone(removed);
    if (command === "resource_update_history") return [];
    if (command === "resource_update_check")
      return {
        root_id: "root-a",
        instance_id: instance.id,
        minecraft_version: instance.minecraft_version,
        loader: "neoforge",
        warnings: [],
        entries: inventory.map((file) => ({ ...file, status: "up_to_date" })),
      };
    if (command === "resource_set_enabled")
      return { changed: 1, undo_id: null, message: "Synthetic change" };
    if (command === "system_info")
      return {
        total_memory_bytes: 16 * 1024 ** 3,
        available_memory_bytes: 8 * 1024 ** 3,
      };
    throw Error(`Unexpected synthetic command ${command}`);
  };
  const props = {
    id: instance.id,
    section: kind,
    api,
    instance,
    scopeKey: "root-a",
    generation: "settings-a:scan-0",
    native: true,
    disabled: false,
    mutationDisabled: false,
    onOpen() {},
    onNotify() {},
    onTaskStart() {},
    settings: {
      root_id: "root-a",
      root: "/synthetic/root-a",
      revision: "settings-a",
      memory_gib: 6,
      overrides: {},
    },
    ...options.props,
  };
  const ui = resourceHarness(
    props,
    {
      isHidden: () => false,
      browseResources: (request) => browsed.push(request),
    },
    options.wrapper,
  );
  return {
    ui,
    props,
    calls,
    browsed,
    reads: () =>
      calls.filter(({ command }) => command === "instance_resources").length,
    inventory(next) {
      inventory = next;
    },
    removed(next) {
      removed = next;
    },
    readOverride(next) {
      readOverride = next;
    },
  };
}
function buttons(ui, key) {
  return elements(
    ui.tree,
    (node) => node.type === "button" && content(node) === ui.i18n.t(key),
  );
}
function button(ui, key) {
  const matches = buttons(ui, key);
  assert.equal(matches.length, 1, `One visible action for ${key}`);
  return matches[0];
}
function rows(ui) {
  return elements(ui.tree, (node) => node.props?.role === "option");
}

for (const kind of ["mods", "resourcepacks", "shaderpacks"]) {
  test(`empty ${kind} can browse compatible resources and refresh an external addition`, async () => {
    const f = fixture(kind);
    try {
      await f.ui.flush();
      button(f.ui, "local.downloadNew").props.onClick();
      assert.deepEqual(f.browsed, [
        {
          section: kind,
          version: "1.21.1",
          loader: kind === "mods" ? "neoforge" : undefined,
        },
      ]);
      f.inventory([resource("external", kind)]);
      button(f.ui, "ui.refresh").props.onClick();
      await f.ui.flush();
      assert.equal(f.reads(), 2);
      assert.equal(rows(f.ui).length, 1);
      assert.ok(content(f.ui.tree).includes("external"));
      button(f.ui, "ui.refresh");
    } finally {
      f.ui.close();
    }
  });
}

for (const kind of ["resourcepacks", "shaderpacks"]) {
  test(`nonempty ${kind} keeps refresh available and retires its old selection`, async () => {
    const f = fixture(kind, [resource("old", kind)]);
    try {
      await f.ui.flush();
      rows(f.ui)[0].props.onClick();
      await f.ui.flush();
      assert.equal(rows(f.ui)[0].props["aria-selected"], true);
      f.inventory([resource("new", kind)]);
      button(f.ui, "ui.refresh").props.onClick();
      await f.ui.flush();
      assert.equal(f.reads(), 2);
      assert.equal(rows(f.ui)[0].props["aria-selected"], false);
      assert.ok(content(f.ui.tree).includes("new"));
      assert.ok(!content(f.ui.tree).includes("old.zip"));
    } finally {
      f.ui.close();
    }
  });
}

test("resource task generations pass through InstancePanel and retire selection and saved actions", async () => {
  const f = fixture("mods", [resource("old")], { wrapper: true });
  try {
    await f.ui.flush();
    assert.equal(f.reads(), 1);
    rows(f.ui)[0].props.onClick();
    await f.ui.flush();
    const refresh = button(f.ui, "ui.refresh").props.onClick,
      browse = button(f.ui, "local.downloadNew").props.onClick;
    f.inventory([resource("new")]);
    f.ui.replace({ resourceRefreshKey: "root-a:1" });
    await f.ui.flush();
    assert.equal(f.reads(), 2);
    assert.ok(content(f.ui.tree).includes("new.jar"));
    assert.equal(rows(f.ui)[0].props["aria-selected"], false);
    refresh();
    browse();
    await f.ui.flush();
    assert.equal(f.reads(), 2);
    assert.deepEqual(f.browsed, []);
    f.ui.replace({
      resourceRefreshKey: "root-a:1",
      instance: { ...f.props.instance, mod_count: 2 },
    });
    await f.ui.flush();
    assert.equal(
      f.reads(),
      2,
      "Same scan key and ordinary progress renders do not rescan",
    );
  } finally {
    f.ui.close();
  }
});

test("a newer scan generation rejects an older pending inventory reply", async () => {
  const f = fixture("resourcepacks", [resource("initial", "resourcepacks")]),
    oldRead = deferred();
  try {
    await f.ui.flush();
    f.readOverride(() => oldRead.promise);
    button(f.ui, "ui.refresh").props.onClick();
    await f.ui.flush();
    f.readOverride(null);
    f.inventory([resource("current", "resourcepacks")]);
    f.ui.replace({ generation: "settings-a:scan-1" });
    await f.ui.flush();
    oldRead.resolve([resource("late", "resourcepacks")]);
    await f.ui.flush();
    assert.equal(f.reads(), 3);
    assert.ok(content(f.ui.tree).includes("current.zip"));
    assert.ok(!content(f.ui.tree).includes("late.zip"));
  } finally {
    f.ui.close();
  }
});

test("saved browse and refresh actions cannot revive after root A to B to A", async () => {
  const f = fixture("resourcepacks", [resource("original", "resourcepacks")]);
  try {
    await f.ui.flush();
    const refresh = button(f.ui, "ui.refresh").props.onClick,
      browse = button(f.ui, "local.downloadNew").props.onClick;
    f.ui.replace({ scopeKey: "root-b" });
    await f.ui.flush();
    f.ui.replace({ scopeKey: "root-a" });
    await f.ui.flush();
    const reads = f.reads();
    refresh();
    browse();
    await f.ui.flush();
    assert.equal(f.reads(), reads);
    assert.deepEqual(f.browsed, []);
    button(f.ui, "local.downloadNew").props.onClick();
    assert.equal(f.browsed.length, 1);
  } finally {
    f.ui.close();
  }
});

test("busy and disabled states reject writes without a disable-to-enable scan loop", async () => {
  const f = fixture("mods", [resource("sample")]);
  try {
    await f.ui.flush();
    const disable = elements(
      f.ui.tree,
      (node) =>
        node.type === "button" &&
        node.props["aria-label"] ===
          f.ui.i18n.t("ui.resourceAction", {
            name: "sample",
            action: f.ui.i18n.t("ui.disable"),
          }),
    )[0];
    assert.ok(disable);
    f.ui.replace({ disabled: true, mutationDisabled: true });
    disable.props.onClick();
    await f.ui.flush();
    assert.equal(
      f.calls.filter(({ command }) => command === "resource_set_enabled")
        .length,
      0,
    );
    f.ui.replace({ disabled: false, mutationDisabled: false });
    await f.ui.flush();
    assert.equal(f.reads(), 1);
    assert.equal(button(f.ui, "local.installFile").props.disabled, false);
    const pending = deferred();
    f.readOverride(() => pending.promise);
    button(f.ui, "ui.refresh").props.onClick();
    await f.ui.flush();
    assert.equal(button(f.ui, "ui.refresh").props.disabled, true);
    button(f.ui, "ui.refresh").props.onClick();
    assert.equal(f.reads(), 2);
    pending.resolve([resource("sample")]);
    await f.ui.flush();
    assert.equal(f.reads(), 2);
  } finally {
    f.ui.close();
  }
});

test("empty recoverable lists show each discovery action once", async () => {
  const f = fixture("resourcepacks");
  try {
    await f.ui.flush();
    f.removed([{ id: "removed-a", files: ["old.zip"], created_at: 1 }]);
    button(f.ui, "ui.refresh").props.onClick();
    await f.ui.flush();
    for (const key of [
      "local.downloadNew",
      "ui.refresh",
      "local.installFile",
      "common.openFolder",
    ])
      button(f.ui, key);
  } finally {
    f.ui.close();
  }
});
