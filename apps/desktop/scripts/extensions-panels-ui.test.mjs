/** Actual bundled entry + Cordis kernel + actual components, with a light
 * deferred native port. No Agent, native application or clipboard is used. */
import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const desktop = fileURLToPath(new URL("../", import.meta.url)),
  require = createRequire(path.join(desktop, "package.json")),
  { build } = require("esbuild"),
  mock = fileURLToPath(
    new URL(
      "../../../experimental/extensions/tests/fixtures/react-events.mjs",
      import.meta.url,
    ),
  );
let compiled, directory, modulePath;
const mounted = new Set(),
  kernels = new Set(),
  subscriptions = new Set();
const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
test.before(async () => {
  globalThis.window = { addEventListener() {}, removeEventListener() {} };
  const scratch = fileURLToPath(
    new URL("../../../work/plugin-panels-2026-10-20/", import.meta.url),
  );
  await mkdir(scratch, { recursive: true });
  directory = await mkdtemp(path.join(scratch, "panels-ui-"));
  const result = await build({
    stdin: {
      contents: `
        export { Kernel } from './src/rh-code/kernel';
        export { default as entry } from './src/rh-code/plugins/extensions.plugin';
        export { createCommunityCatalogPort } from './src/rh-code/communityCatalog';
        export { ExperimentalCards } from './src/ExperimentalExtensions';
        export { ExtensionActions } from './src/ExtensionActions';
        export { ExtensionFields } from './src/ExtensionFields';
        export * from './src/extensionEvents';
        export { t } from './src/i18n';
        export { mount, nodes, text } from ${JSON.stringify(mock)};
      `,
      resolveDir: desktop,
      loader: "tsx",
    },
    plugins: [
      {
        name: "view-controller-boundary",
        setup(builder) {
          builder.onResolve(
            { filter: /^\.\.\/controller$/ },
            ({ importer }) => {
              if (importer.endsWith("extensions.plugin.tsx"))
                return { path: "test-controller", namespace: "test" };
            },
          );
          builder.onLoad({ filter: /.*/, namespace: "test" }, () => ({
            contents:
              "export const useCode = () => globalThis.__panelTestCode;",
          }));
        },
      },
    ],
    bundle: true,
    platform: "node",
    format: "cjs",
    jsx: "automatic",
    alias: { react: mock, "react/jsx-runtime": mock, "react-dom": mock },
    loader: { ".css": "empty" },
    write: false,
  });
  modulePath = path.join(directory, "panels.cjs");
  await writeFile(modulePath, result.outputFiles[0].text);
  compiled = require(modulePath);
});
test.afterEach(async () => {
  for (const harness of mounted) harness.unmount();
  mounted.clear();
  for (const kernel of kernels) await kernel.dispose();
  kernels.clear();
  for (const unsubscribe of subscriptions) unsubscribe();
  subscriptions.clear();
  delete globalThis.__panelTestCode;
});
test.after(async () => {
  if (previousWindow)
    Object.defineProperty(globalThis, "window", previousWindow);
  else delete globalThis.window;
  if (modulePath) delete require.cache[modulePath];
  if (directory) await rm(directory, { recursive: true, force: true });
});
function api() {
  const calls = [];
  const invoke = (command, payload) =>
    new Promise((resolve, reject) =>
      calls.push({
        operation:
          command === "experimental_call" ? payload.operation : command,
        args: command === "experimental_call" ? payload.args : payload,
        resolve,
        reject,
        taken: false,
      }),
    );
  invoke.next = (operation) => {
    const call = calls.find(
      (item) => item.operation === operation && !item.taken,
    );
    assert.ok(call, `Missing ${operation}`);
    call.taken = true;
    return call;
  };
  invoke.count = (operation) =>
    calls.filter((item) => item.operation === operation).length;
  return invoke;
}
function mount(Component, props) {
  const harness = compiled.mount(Component, props);
  mounted.add(harness);
  return harness;
}
const settle = async () => {
  for (let i = 0; i < 12; i++) await new Promise((done) => setImmediate(done));
};
const panel = (name = "a", overrides = {}) => ({
  viewId: `rh.extensions.panel.${name.repeat(64)}`,
  extensionId: `example.${name}`,
  cardId: "notes",
  title: `Notes ${name}`,
  extensionName: `Plugin ${name}`,
  digest: "d".repeat(64),
  generation: 1,
  ...overrides,
});
const action = (name = "a", overrides = {}) => ({
  extensionId: `example.${name}`,
  extensionName: `Plugin ${name}`,
  cardId: "notes",
  cardTitle: `Notes ${name}`,
  id: "open",
  label: `Open ${name}`,
  kind: "navigate",
  enabled: true,
  unavailableReason: null,
  digest: "d".repeat(64),
  generation: 7,
  ...overrides,
});
const card = (name = "a", overrides = {}) => ({
  extensionId: `example.${name}`,
  extensionName: `Plugin ${name}`,
  id: "notes",
  title: `Notes ${name}`,
  text: "Saved settings",
  digest: "d".repeat(64),
  generation: 7,
  fields: [
    {
      id: "notes",
      label: "Notes",
      type: "text",
      maxLength: 80,
      default: "",
      value: `Saved ${name}`,
    },
  ],
  actions: [{ id: "open", label: "Open", enabled: true }],
  ...overrides,
});
const filter = (name) => ({ extensionId: `example.${name}`, cardId: "notes" });
const props = (invoke, extra = {}) => ({
  api: invoke,
  native: true,
  slot: "rh-code.cards",
  onNavigate() {},
  ...extra,
});
function button(harness, label, index = 0) {
  const found = compiled
    .nodes(harness.tree)
    .filter((node) => node.type === "button" && compiled.text(node) === label)[
    index
  ];
  assert.ok(found, `Missing ${label}`);
  return found.props;
}
function form(harness) {
  const found = compiled
    .nodes(harness.tree)
    .find((node) => node.type === compiled.ExtensionFields);
  assert.ok(found, "Missing fields");
  return found.props;
}
const search = (harness) =>
  compiled
    .nodes(harness.tree)
    .find((node) => node.type === "input" && node.props.type === "search")
    .props;
const run = (harness, index = 0) =>
  button(harness, compiled.t("experimental.actionRun"), index);
const enter = (harness, composing = false) =>
  search(harness).onKeyDown({
    key: "Enter",
    nativeEvent: { isComposing: composing },
    preventDefault() {},
  });
async function actions(invoke, values = [action()], extra = {}) {
  const harness = mount(compiled.ExtensionActions, props(invoke, extra));
  invoke.next("extensions_catalog").resolve({ panels: [], actions: values });
  await harness.flush();
  return harness;
}
const manifest = () => ({
  id: "rh.extensions",
  name: "Community panels",
  version: "1.1.0",
  required: false,
  scope: "rh-code",
  dependencies: [],
  capabilities: [],
  views: ["rh.extensions.cards", "rh.extensions.actions"],
  dynamicViews: { prefix: "rh.extensions.panel.", limit: 384 },
});
function kernelWithPort(port) {
  let imports = 0;
  const kernel = new compiled.Kernel([
    {
      manifest: manifest(),
      runtime: { community: port.runtime },
      load: async () => {
        imports++;
        return compiled.entry;
      },
    },
  ]);
  kernels.add(kernel);
  return { kernel, imports: () => imports };
}

test("real scoped module stays cold, refreshes dynamic metadata without changing render identity, and Cordis cleanup unsubscribes", async () => {
  const invoke = api(),
    port = compiled.createCommunityCatalogPort({
      api: invoke,
      native: true,
      visible: true,
    }),
    { kernel, imports } = kernelWithPort(port);
  await kernel.configure([], ["launcher"]);
  compiled.notifyExtensionsChanged();
  assert.equal(imports(), 0);
  assert.equal(invoke.count("extensions_catalog"), 0);
  await kernel.configure([], ["rh-code"]);
  assert.equal(imports(), 1);
  invoke
    .next("extensions_catalog")
    .resolve({ panels: [panel(), panel("b")], actions: [] });
  await settle();
  const first = kernel.views.get(panel().viewId).render,
    second = kernel.views.get(panel("b").viewId).render;
  assert.equal(kernel.views.get(panel().viewId).owner, "rh.extensions");
  assert.equal(kernel.views.get(panel().viewId).label, "Notes a · Plugin a");
  compiled.notifyExtensionsChanged();
  assert.equal(kernel.views.get(panel("b").viewId).render, second);
  invoke.next("extensions_catalog").resolve({
    panels: [
      panel("a", { title: "Updated title", generation: 8 }),
      panel("b", { generation: 8 }),
    ],
    actions: [],
  });
  await settle();
  assert.equal(kernel.views.get(panel().viewId).render, first);
  assert.equal(kernel.views.get(panel("b").viewId).render, second);
  assert.equal(
    kernel.views.get(panel().viewId).label,
    "Updated title · Plugin a",
  );
  const element = first();
  assert.deepEqual(element.props.filter, filter("a"));
  globalThis.__panelTestCode = { api: invoke, native: true };
  const view = mount(element.type, element.props);
  const child = compiled
    .nodes(view.tree)
    .find((node) => node.type === compiled.ExperimentalCards);
  assert.deepEqual(child.props.filter, filter("a"));
  assert.equal(child.props.slot, "rh-code.cards");
  compiled.notifyExtensionsChanged();
  const late = invoke.next("extensions_catalog");
  await kernel.configure(["rh.extensions"], ["rh-code"]);
  late.resolve({ panels: [panel()], actions: [] });
  await settle();
  assert.equal(kernel.views.size, 0);
  const before = invoke.count("extensions_catalog");
  compiled.notifyExtensionsChanged();
  port.publishSurface();
  assert.equal(invoke.count("extensions_catalog"), before);
  await kernel.configure([], ["rh-code"]);
  invoke.next("extensions_catalog").resolve({ panels: [panel()], actions: [] });
  await settle();
  assert.notEqual(kernel.views.get(panel().viewId).render, first);
});

test("hidden and replaced host surfaces synchronously retire reads and only effect publication changes the registry", async () => {
  const invoke = api(),
    other = api(),
    port = compiled.createCommunityCatalogPort({
      api: invoke,
      native: true,
      visible: false,
    }),
    { kernel } = kernelWithPort(port);
  await kernel.configure([], ["rh-code"]);
  assert.equal(invoke.count("extensions_catalog"), 0);
  port.setSurface({ api: invoke, native: true, visible: true });
  port.publishSurface();
  const old = invoke.next("extensions_catalog");
  port.setSurface({ api: invoke, native: true, visible: false });
  port.setSurface({ api: invoke, native: true, visible: true });
  old.resolve({ panels: [panel()], actions: [] });
  await settle();
  assert.equal(kernel.views.has(panel().viewId), false);
  port.publishSurface();
  invoke.next("extensions_catalog").resolve({ panels: [panel()], actions: [] });
  await settle();
  const render = kernel.views.get(panel().viewId).render;
  port.setSurface({ api: other, native: false, visible: true });
  assert.equal(
    kernel.views.get(panel().viewId).render,
    render,
    "render retirement cannot notify React registry listeners",
  );
  port.publishSurface();
  assert.equal(kernel.views.has(panel().viewId), false);
  assert.equal(other.count("extensions_catalog"), 0);
  port.setSurface({ api: other, native: true, visible: true });
  port.publishSurface();
  const stale = other.next("extensions_catalog");
  compiled.notifyExtensionsChanged();
  stale.resolve({ panels: [panel()], actions: [] });
  other
    .next("extensions_catalog")
    .resolve({ panels: [panel("b")], actions: [] });
  await settle();
  assert.equal(kernel.views.has(panel().viewId), false);
  assert.equal(kernel.views.has(panel("b").viewId), true);
  port.retire();
  assert.equal(kernel.views.has(panel("b").viewId), false);
});

test("an extension publication cannot reopen an Api surface that is still waiting for the host effect", async () => {
  const first = api(),
    second = api(),
    states = [],
    port = compiled.createCommunityCatalogPort({
      api: first,
      native: true,
      visible: true,
    });
  subscriptions.add(
    port.runtime.subscribe(() => states.push(port.runtime.isAvailable())),
  );
  port.setSurface({ api: second, native: true, visible: true });
  assert.equal(port.runtime.isAvailable(), false);
  compiled.notifyExtensionsChanged();
  assert.equal(port.runtime.isAvailable(), false);
  await assert.rejects(port.runtime.readCatalog(), /unavailable/);
  assert.equal(second.count("extensions_catalog"), 0);
  assert.deepEqual(states, [false]);
  port.publishSurface();
  assert.equal(port.runtime.isAvailable(), true);
  assert.deepEqual(states, [false, true]);
  const read = port.runtime.readCatalog();
  second.next("extensions_catalog").resolve({ panels: [], actions: [] });
  assert.deepEqual(await read, { panels: [], actions: [] });
  assert.equal(first.count("extensions_catalog"), 0);
});

test("a surface event between synchronous kernel retirement and Cordis cleanup cannot throw or revive views", async () => {
  const invoke = api(),
    port = compiled.createCommunityCatalogPort({
      api: invoke,
      native: true,
      visible: true,
    }),
    { kernel } = kernelWithPort(port);
  await kernel.configure([], ["rh-code"]);
  invoke.next("extensions_catalog").resolve({ panels: [panel()], actions: [] });
  await settle();
  compiled.notifyExtensionsChanged();
  const old = invoke.next("extensions_catalog");
  const retiring = kernel.configure(["rh.extensions"], ["rh-code"]);
  assert.doesNotThrow(() => port.retire());
  old.resolve({ panels: [panel()], actions: [] });
  await retiring;
  await settle();
  assert.equal(kernel.views.size, 0);
  port.setSurface({ api: invoke, native: true, visible: true });
  port.publishSurface();
  const calls = invoke.count("extensions_catalog");
  compiled.notifyExtensionsChanged();
  assert.equal(invoke.count("extensions_catalog"), calls);
  await kernel.configure([], ["rh-code"]);
  invoke
    .next("extensions_catalog")
    .resolve({ panels: [panel("b")], actions: [] });
  await settle();
  assert.equal(kernel.views.has(panel("b").viewId), true);
});

test("cancelled queued disable preserves its active provider and resumes catalog reads after hide and return", async () => {
  const invoke = api(),
    port = compiled.createCommunityCatalogPort({
      api: invoke,
      native: true,
      visible: true,
    }),
    { kernel, imports } = kernelWithPort(port);
  await kernel.configure([], ["rh-code"]);
  invoke.next("extensions_catalog").resolve({ panels: [panel()], actions: [] });
  await settle();
  assert.equal(invoke.count("extensions_catalog"), 1);
  const disabling = kernel.configure(["rh.extensions"], ["rh-code"]);
  port.setSurface({ api: invoke, native: true, visible: false });
  assert.doesNotThrow(() => port.publishSurface());
  // A newer configuration cancels disable before the queued reconciliation
  // disposes the active Cordis fiber. Rejected publication was only temporary.
  const reenabling = kernel.configure([], ["rh-code"]);
  port.setSurface({ api: invoke, native: true, visible: true });
  port.publishSurface();
  assert.equal(invoke.count("extensions_catalog"), 2);
  invoke
    .next("extensions_catalog")
    .resolve({ panels: [panel("b")], actions: [] });
  await Promise.all([disabling, reenabling]);
  await settle();
  assert.equal(
    imports(),
    1,
    "The surviving active module must not need reimporting",
  );
  assert.equal(kernel.views.has(panel().viewId), false);
  assert.equal(kernel.views.has(panel("b").viewId), true);
  compiled.notifyExtensionsChanged();
  assert.equal(invoke.count("extensions_catalog"), 3);
  invoke.next("extensions_catalog").resolve({ panels: [], actions: [] });
  await settle();
  assert.equal(kernel.views.has(panel("b").viewId), false);
});

test("catalog failure preserves mounted panel identities and exposes the aggregate error and explicit refresh recovery", async () => {
  const invoke = api(),
    port = compiled.createCommunityCatalogPort({
      api: invoke,
      native: true,
      visible: true,
    }),
    { kernel } = kernelWithPort(port);
  await kernel.configure([], ["rh-code"]);
  invoke.next("extensions_catalog").resolve({ panels: [panel()], actions: [] });
  await settle();
  const first = kernel.views.get(panel().viewId).render;
  globalThis.__panelTestCode = { api: invoke, native: true };
  const aggregateElement = kernel.views.get("rh.extensions.cards").render(),
    aggregate = mount(aggregateElement.type, aggregateElement.props);
  const cardsElement = compiled
      .nodes(aggregate.tree)
      .find((node) => node.type === compiled.ExperimentalCards),
    cards = mount(cardsElement.type, cardsElement.props);
  invoke.next("extensions_cards").resolve([card()]);
  await cards.flush();
  compiled.notifyExtensionsChanged();
  await cards.flush();
  invoke
    .next("extensions_catalog")
    .reject(new Error("Synthetic metadata failure"));
  await settle();
  await aggregate.flush();
  assert.ok(
    compiled.text(aggregate.tree).includes("Synthetic metadata failure"),
  );
  assert.equal(kernel.views.get(panel().viewId).render, first);
  button(cards, compiled.t("experimental.refreshPanels")).onClick();
  await cards.flush();
  invoke
    .next("extensions_catalog")
    .resolve({ panels: [panel("a", { title: "Recovered" })], actions: [] });
  await settle();
  await aggregate.flush();
  assert.ok(
    !compiled.text(aggregate.tree).includes("Synthetic metadata failure"),
  );
  assert.equal(kernel.views.get(panel().viewId).render, first);
  assert.equal(kernel.views.get(panel().viewId).label, "Recovered · Plugin a");
});

test("a host Api replacement retires static action handlers even when the controller keeps the same public Api wrapper", async () => {
  const first = api(),
    second = api(),
    navigations = [];
  let active = first;
  const stableApi = (command, payload) => active(command, payload),
    port = compiled.createCommunityCatalogPort({
      api: first,
      native: true,
      visible: true,
    }),
    kernel = new compiled.Kernel([
      {
        manifest: manifest(),
        runtime: {
          community: port.runtime,
          navigate: (target) => navigations.push(target),
        },
        load: async () => compiled.entry,
      },
    ]);
  kernels.add(kernel);
  await kernel.configure([], ["rh-code"]);
  first.next("extensions_catalog").resolve({ panels: [], actions: [] });
  await settle();
  globalThis.__panelTestCode = { api: stableApi, native: true };
  const element = kernel.views.get("rh.extensions.actions").render(),
    wrapper = mount(element.type, element.props),
    childProps = () =>
      compiled
        .nodes(wrapper.tree)
        .find((node) => node.type === compiled.ExtensionActions).props,
    harness = mount(compiled.ExtensionActions, childProps());
  first.next("extensions_catalog").resolve({ panels: [], actions: [action()] });
  await harness.flush();
  const stale = run(harness).onClick;
  stale();
  const pending = first.next("extensions_action");
  active = second;
  port.setSurface({ api: second, native: true, visible: true });
  wrapper.setProps(element.props); // Host render, before its notification effect.
  assert.equal(childProps().api, stableApi);
  assert.equal(childProps().native, false);
  harness.setProps(childProps());
  stale();
  pending.resolve({ kind: "navigate", target: "settings" });
  await harness.flush();
  assert.deepEqual(navigations, []);
  assert.equal(second.count("extensions_action"), 0);
  port.publishSurface();
  await wrapper.flush();
  assert.equal(childProps().native, true);
  harness.setProps(childProps());
  second.next("extensions_catalog").resolve({ panels: [], actions: [] });
  second
    .next("extensions_catalog")
    .resolve({ panels: [], actions: [action("b", { generation: 8 })] });
  await harness.flush();
  await settle();
  stale();
  assert.equal(second.count("extensions_action"), 0);
  run(harness).onClick();
  assert.equal(second.next("extensions_action").args.expected.generation, 8);
});

test("independent filtered panels preserve B's dirty draft when A saves and equal filter objects do not restart reads", async () => {
  const invoke = api(),
    first = mount(
      compiled.ExperimentalCards,
      props(invoke, { filter: filter("a") }),
    ),
    second = mount(
      compiled.ExperimentalCards,
      props(invoke, { filter: filter("b") }),
    );
  for (const [name, harness] of [
    ["a", first],
    ["b", second],
  ]) {
    const read = invoke.next("extensions_cards");
    assert.deepEqual(read.args, {
      slot: "rh-code.cards",
      filter: filter(name),
    });
    read.resolve([card(name)]);
    await harness.flush();
  }
  form(second).onChange("notes", "Unsaved B");
  await second.flush();
  second.setProps(props(invoke, { filter: { ...filter("b") } }));
  assert.equal(invoke.count("extensions_cards"), 2);
  assert.equal(form(second).draft.notes, "Unsaved B");
  form(first).onChange("notes", "Saved A");
  await first.flush();
  button(first, compiled.t("experimental.settingsSave")).onClick();
  const save = invoke.next("extensions_settings");
  assert.deepEqual(save.args, {
    id: "example.a",
    expected: { digest: "d".repeat(64), generation: 7 },
    values: { notes: "Saved A" },
  });
  save.resolve({ generation: 8 });
  await first.flush();
  await second.flush();
  for (const [name, harness] of [
    ["a", first],
    ["b", second],
  ]) {
    const nextCard = card(name, { generation: 8 });
    if (name === "a") nextCard.fields[0].value = "Saved A";
    invoke.next("extensions_cards").resolve([nextCard]);
    await harness.flush();
  }
  assert.equal(form(first).draft.notes, "Saved A");
  assert.equal(
    button(first, compiled.t("experimental.settingsSave")).disabled,
    true,
  );
  assert.equal(form(second).draft.notes, "Unsaved B");
  assert.equal(
    button(second, compiled.t("experimental.settingsSave")).disabled,
    false,
  );
  assert.equal(button(second, "Open").disabled, true);
});

test("filter A -> B -> A retires old forms, drafts and pending actions while unrelated filters stay isolated", async () => {
  const invoke = api(),
    navigations = [],
    initial = props(invoke, {
      filter: filter("a"),
      onNavigate: (target) => navigations.push(target),
    }),
    harness = mount(compiled.ExperimentalCards, initial);
  invoke.next("extensions_cards").resolve([card()]);
  await harness.flush();
  const oldFields = form(harness).onChange;
  oldFields("notes", "Unsaved A");
  await harness.flush();
  const oldSave = button(
    harness,
    compiled.t("experimental.settingsSave"),
  ).onClick;
  harness.setProps(props(invoke, { filter: filter("b") }));
  const pendingB = invoke.next("extensions_cards");
  harness.setProps(initial);
  oldSave();
  oldFields("notes", "Retired A");
  assert.equal(invoke.count("extensions_settings"), 0);
  pendingB.resolve([card("b")]);
  invoke.next("extensions_cards").resolve([card()]);
  await harness.flush();
  assert.equal(form(harness).draft.notes, "Saved a");
  button(harness, "Open").onClick();
  const pendingAction = invoke.next("extensions_action");
  harness.setProps(props(invoke, { filter: filter("b") }));
  harness.setProps(initial);
  pendingAction.resolve({ kind: "navigate", target: "settings" });
  await harness.flush();
  assert.deepEqual(navigations, []);
});

test("action search has no RPC side effects and Enter executes only one current enabled result with exact expected identity", async () => {
  const invoke = api(),
    harness = await actions(invoke, [
      action(),
      action("b"),
      action("c", {
        enabled: false,
        unavailableReason: "no-selected-instance",
      }),
    ]);
  assert.ok(
    compiled
      .text(harness.tree)
      .includes(compiled.t("experimental.savedActionsHelp")),
  );
  assert.ok(compiled.text(harness.tree).includes("Plugin a · Notes a"));
  assert.equal(run(harness, 2).disabled, true);
  enter(harness);
  assert.equal(invoke.count("extensions_action"), 0);
  const oldButton = run(harness).onClick;
  search(harness).onChange({ target: { value: "PLUGIN B" } });
  oldButton();
  await harness.flush();
  assert.equal(invoke.count("extensions_catalog"), 1);
  assert.equal(invoke.count("extensions_action"), 0);
  assert.equal(
    compiled
      .nodes(harness.tree)
      .filter(
        (node) =>
          node.type === "button" &&
          compiled.text(node) === compiled.t("experimental.actionRun"),
      ).length,
    1,
  );
  enter(harness, true);
  assert.equal(invoke.count("extensions_action"), 0);
  enter(harness);
  enter(harness);
  const call = invoke.next("extensions_action");
  assert.deepEqual(call.args, {
    extensionId: "example.b",
    cardId: "notes",
    actionId: "open",
    expected: { digest: "d".repeat(64), generation: 7 },
  });
  assert.equal(invoke.count("extensions_action"), 1);
  call.resolve({ kind: "text-copied", extensionId: "example.b" });
  await harness.flush();
  assert.ok(
    compiled.text(harness.tree).includes(compiled.t("experimental.textCopied")),
  );
  search(harness).onChange({ target: { value: "Plugin c" } });
  await harness.flush();
  enter(harness);
  run(harness).onClick();
  assert.equal(invoke.count("extensions_action"), 1);
  assert.ok(
    compiled
      .text(harness.tree)
      .includes(compiled.t("experimental.actionNoInstance")),
  );
});

test("action refresh and host retirement reject captured handlers and late navigation while late committed appearance still broadcasts", async () => {
  for (const boundary of [
    "refresh",
    "search",
    "api",
    "hide",
    "effects",
    "unmount",
  ]) {
    const invoke = api(),
      other = api(),
      navigations = [],
      events = [],
      initial = props(invoke, {
        onNavigate: (target) => navigations.push(target),
      }),
      harness = await actions(invoke, [action()], initial);
    const unsubscribe = compiled.subscribeLauncherAppearanceApplied((...args) =>
      events.push(args),
    );
    subscriptions.add(unsubscribe);
    const stale = run(harness).onClick;
    stale();
    const pending = invoke.next("extensions_action");
    if (boundary === "refresh") compiled.notifyExtensionsChanged();
    if (boundary === "search")
      search(harness).onChange({ target: { value: "missing" } });
    if (boundary === "api") harness.setProps(props(other));
    if (boundary === "hide") {
      harness.setProps({ ...initial, native: false });
      harness.setProps(initial);
    }
    if (boundary === "effects") harness.restartEffects();
    if (boundary === "unmount") harness.unmount();
    stale();
    pending.resolve({ kind: "navigate", target: "settings" });
    await harness.flush();
    assert.deepEqual(navigations, [], boundary);
    assert.equal(invoke.count("extensions_action"), 1, boundary);
    assert.equal(other.count("extensions_action"), 0, boundary);
    assert.equal(harness.retiredWrites, 0, boundary);
    harness.unmount();
    unsubscribe();
    subscriptions.delete(unsubscribe);
  }
  const invoke = api(),
    events = [],
    harness = await actions(invoke);
  subscriptions.add(
    compiled.subscribeLauncherAppearanceApplied((...args) => events.push(args)),
  );
  run(harness).onClick();
  const pending = invoke.next("extensions_action");
  harness.unmount();
  pending.resolve({
    kind: "launcher-appearance-applied",
    extensionId: "example.a",
  });
  await harness.flush();
  assert.deepEqual(events, [[]]);
  assert.equal(harness.retiredWrites, 0);
});

test("catalog responses are retired before replacement, failures can refresh, and summary completion uses the ordinary trusted display", async () => {
  const invoke = api(),
    harness = mount(compiled.ExtensionActions, props(invoke));
  const old = invoke.next("extensions_catalog");
  compiled.notifyExtensionsChanged();
  await harness.flush();
  old.resolve({ panels: [], actions: [action("a")] });
  invoke
    .next("extensions_catalog")
    .reject(new Error("Synthetic catalog failure"));
  await harness.flush();
  assert.ok(compiled.text(harness.tree).includes("Synthetic catalog failure"));
  assert.equal(
    compiled
      .nodes(harness.tree)
      .filter(
        (node) =>
          node.type === "button" &&
          compiled.text(node) === compiled.t("experimental.actionRun"),
      ).length,
    0,
  );
  compiled.notifyExtensionsChanged();
  await harness.flush();
  invoke.next("extensions_catalog").resolve({
    panels: [],
    actions: [action("b", { kind: "show-instance-summary" })],
  });
  await harness.flush();
  run(harness).onClick();
  invoke.next("extensions_action").resolve({
    kind: "show-instance-summary",
    summary: {
      minecraftVersion: "1.21.1",
      loader: "Fabric",
      modCount: 12,
      isolated: true,
    },
  });
  await harness.flush();
  assert.ok(compiled.text(harness.tree).includes("1.21.1"));
  assert.ok(compiled.text(harness.tree).includes("Fabric"));
  const close = button(harness, compiled.t("common.cancel")).onClick;
  compiled.notifyExtensionsChanged();
  close();
  await harness.flush();
  assert.ok(!compiled.text(harness.tree).includes("1.21.1"));
});
