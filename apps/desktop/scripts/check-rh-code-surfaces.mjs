/** Actual product shell, community wrappers and request scopes, with only the
 * unrelated editor views/controller seam replaced. Synthetic replies never use
 * the native host, provider, chooser or project store. */
import assert from "node:assert/strict";
import test from "node:test";
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import * as hooks from "../../../experimental/extensions/tests/fixtures/react-events.mjs";

const require = createRequire(import.meta.url),
  ts = require("typescript"),
  source = fileURLToPath(new URL("../src/", import.meta.url));

function fixture(t, initial = {}) {
  const contexts = [],
    modules = new Map(),
    mounted = new Set(),
    demands = [],
    calls = [];
  const react = {
    ...hooks,
    createContext(value) {
      const context = { value, Provider: Symbol("Provider") };
      contexts.push(context);
      return context;
    },
    useContext: (context) => context.value,
  };
  const api = (command, payload) =>
    new Promise((resolve, reject) => {
      calls.push({
        operation:
          command === "experimental_call" ? payload.operation : command,
        args: command === "experimental_call" ? payload.args : payload,
        resolve,
        reject,
        taken: false,
      });
    });
  const next = (operation) => {
    const call = calls.find(
      (call) => call.operation === operation && !call.taken,
    );
    assert.ok(call, `Missing ${operation}`);
    call.taken = true;
    return call;
  };
  const code = {
    api,
    native: true,
    snapshot: null,
    project: null,
    error: "",
    setError() {},
    refreshEngine: async () => {},
    run() {},
    stop() {},
  };
  const CodeContext = react.createContext(null);
  const controller = {
    CodeContext,
    useCode: () => react.useContext(CodeContext),
    useController: (_, native, engineRequested) => {
      demands.push(engineRequested);
      return { ...code, native };
    },
  };
  const marker = (name) => ({ [name]: function Stub() {} });
  const stubs = {
    CeSelect: marker("CeSelect"),
    ExperimentalAi: marker("ExperimentalAi"),
    ExperimentalVersion: marker("ExperimentalVersion"),
    instanceOperationUi: marker("InstanceOperationDialog"),
    "rh-code/LegacyArchive": marker("LegacyArchive"),
    "rh-code/Projects": marker("Projects"),
    "rh-code/Dialog": marker("Dialog"),
    "rh-code/Workspace": marker("Workspace"),
    "rh-code/Layouts": marker("Layouts"),
  };
  function load(file) {
    if (modules.has(file)) return modules.get(file).exports;
    const module = { exports: {} };
    modules.set(file, module);
    const compiled = ts.transpileModule(readFileSync(file, "utf8"), {
      fileName: file,
      compilerOptions: {
        target: ts.ScriptTarget.ES2022,
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.ReactJSX,
      },
    }).outputText;
    const localRequire = (id) => {
      if (id === "react") return react;
      if (id === "react/jsx-runtime") return hooks;
      if (id === "lucide-react")
        return new Proxy({}, { get: () => () => null });
      if (!id.startsWith(".")) return require(id);
      if (id.endsWith(".css")) return {};
      const target = path.resolve(path.dirname(file), id),
        relative = path.relative(source, target);
      if (relative === "rh-code/controller") return controller;
      if (relative === "rh-code/locale")
        return { label: (_, english) => english };
      if (relative === "i18n") return { t: (id) => id, serviceError: String };
      if (stubs[relative]) return stubs[relative];
      return load(target + (existsSync(target + ".tsx") ? ".tsx" : ".ts"));
    };
    new Function("require", "module", "exports", compiled)(
      localRequire,
      module,
      module.exports,
    );
    return module.exports;
  }
  const { RHCode } = load(path.join(source, "rh-code/RHCode.tsx")),
    { CommunityCards } = load(
      path.join(source, "rh-code/plugins/extensions.plugin.tsx"),
    ),
    { ExperimentalCards, ExperimentalExtensions } = load(
      path.join(source, "ExperimentalExtensions.tsx"),
    ),
    { Plugins } = load(path.join(source, "rh-code/Plugins.tsx"));
  const mount = (Component, props) => {
    const harness = hooks.mount(Component, props);
    mounted.add(harness);
    return harness;
  };
  t.after(() => {
    for (const harness of mounted) harness.unmount();
  });
  let props = {
    api,
    native: true,
    visible: true,
    section: "plugins",
    onNavigate() {},
    kernel: { subscribe: () => () => {} },
    onController() {},
    onConfiguration() {},
    ...initial,
  };
  const shell = mount(RHCode, props);
  function publish() {
    for (const node of hooks.nodes(shell.tree)) {
      const context = contexts.find(
        (context) => context.Provider === node.type,
      );
      if (context) context.value = node.props.value;
    }
  }
  publish();
  return {
    api,
    calls,
    next,
    demands,
    mount,
    shell,
    CommunityCards,
    ExperimentalCards,
    ExperimentalExtensions,
    Plugins,
    visible(visible) {
      this.update({ visible });
    },
    update(patch) {
      props = { ...props, ...patch };
      shell.setProps(props);
      publish();
    },
    async flush() {
      await shell.flush();
      publish();
    },
  };
}
function node(harness, type) {
  const found = hooks.nodes(harness.tree).find((node) => node.type === type);
  assert.ok(found, "Missing child component");
  return found;
}
function button(harness, label) {
  const found = hooks
    .nodes(harness.tree)
    .find((node) => node.type === "button" && hooks.text(node) === label);
  assert.ok(found, `Missing button ${label}`);
  return found.props;
}
const card = {
  extensionId: "example.notes",
  extensionName: "Example notes",
  id: "notes",
  digest: "a".repeat(64),
  generation: 7,
  scope: "rh-code",
  title: "Notes",
  text: "Card",
  actions: [{ id: "go", label: "Go", enabled: true }],
};

test("the product shell requests AI on Code entry or explicit settings, and publishes surface visibility", async (t) => {
  const view = fixture(t);
  assert.equal(view.demands.at(-1), false);
  view.update({ section: "code" });
  assert.equal(view.demands.at(-1), true);
  view.visible(false);
  assert.equal(view.demands.at(-1), false);
  view.update({ section: "plugins", visible: true });
  assert.equal(view.demands.at(-1), false);
  const settings = hooks
    .nodes(view.shell.tree)
    .find(
      (node) =>
        node.type === "button" &&
        node.props["aria-label"] === "RH Code settings",
    );
  settings.props.onClick();
  await view.flush();
  assert.equal(view.demands.at(-1), true);
});

for (const returnBeforeReply of [false, true]) {
  test(`community card intents retire on surface departure${returnBeforeReply ? " and remain retired after return" : ""}`, async (t) => {
    const view = fixture(t, { section: "code" }),
      destinations = [];
    const properties = { onNavigate: (target) => destinations.push(target) };
    const wrapper = view.mount(view.CommunityCards, properties),
      cards = view.mount(
        view.ExperimentalCards,
        node(wrapper, view.ExperimentalCards).props,
      );
    view.next("extensions_cards").resolve([card]);
    await cards.flush();
    const saved = button(cards, "Go").onClick;
    saved();
    const old = view.next("extensions_action");
    function visible(value) {
      view.visible(value);
      wrapper.setProps(properties);
      cards.setProps(node(wrapper, view.ExperimentalCards).props);
    }
    visible(false);
    assert.equal(node(wrapper, view.ExperimentalCards).props.native, false);
    saved();
    assert.equal(
      view.calls.filter((call) => call.operation === "extensions_action")
        .length,
      1,
    );
    if (returnBeforeReply) {
      visible(true);
      view.next("extensions_cards").resolve([card]);
      await cards.flush();
    }
    old.resolve({ kind: "navigate", target: "downloads" });
    await cards.flush();
    assert.deepEqual(destinations, []);
    if (!returnBeforeReply) {
      visible(true);
      view.next("extensions_cards").resolve([card]);
      await cards.flush();
    }
    button(cards, "Go").onClick();
    view
      .next("extensions_action")
      .resolve({ kind: "navigate", target: "instances" });
    await cards.flush();
    assert.deepEqual(destinations, ["instances"]);
    assert.equal(cards.retiredWrites, 0);
  });
}

test("the retained Plugins manager retires an old chooser even after hiding and returning", async (t) => {
  const view = fixture(t),
    properties = { kernel: {}, onNavigate() {} },
    wrapper = view.mount(view.Plugins, properties);
  button(wrapper, "Community plugins").onClick();
  await wrapper.flush();
  const manager = view.mount(
    view.ExperimentalExtensions,
    node(wrapper, view.ExperimentalExtensions).props,
  );
  view
    .next("extensions_list")
    .resolve({ entries: [], safeMode: false, warning: null });
  await manager.flush();
  button(manager, "experimental.importExtension").onClick();
  const chooser = view.next("experimental_choose");
  for (const visible of [false, true]) {
    view.visible(visible);
    wrapper.setProps(properties);
    assert.equal(
      node(wrapper, view.ExperimentalExtensions).props.native,
      visible,
    );
    manager.setProps(node(wrapper, view.ExperimentalExtensions).props);
  }
  chooser.resolve({ status: "selected", path: "synthetic.json" });
  await manager.flush();
  assert.equal(
    view.calls.some((call) => call.operation === "extensions_review"),
    false,
  );
  view
    .next("extensions_list")
    .resolve({ entries: [], safeMode: false, warning: null });
  await manager.flush();
  button(manager, "experimental.importExtension").onClick();
  assert.ok(view.next("experimental_choose"));
  assert.equal(manager.retiredWrites, 0);
});
