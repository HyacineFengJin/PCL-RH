/** Real launcher plugin components with the deterministic React event fixture.
 * Native replies are deferred synthetic values; no Agent or native effects run. */
import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const desktop = fileURLToPath(new URL("../", import.meta.url));
const require = createRequire(path.join(desktop, "package.json"));
const { build } = require("esbuild"),
  ts = require("typescript");
const mock = fileURLToPath(
  new URL(
    "../../../experimental/extensions/tests/fixtures/react-events.mjs",
    import.meta.url,
  ),
);
const newSlots = [
  "home.banner",
  "home.quick-actions",
  "settings.launch",
  "settings.java",
  "settings.manage",
  "settings.language",
  "settings.misc",
];
let compiled, directory, modulePath;
const mounted = new Set(),
  subscriptions = new Set();
const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");

test.before(async () => {
  globalThis.window = { addEventListener() {}, removeEventListener() {} };
  const scratch = fileURLToPath(
    new URL(
      "../../../work/plugin-launcher-surfaces-2026-10-20/",
      import.meta.url,
    ),
  );
  await mkdir(scratch, { recursive: true });
  directory = await mkdtemp(path.join(scratch, "launcher-ui-"));
  const result = await build({
    stdin: {
      contents: `
        export { ExperimentalCards } from './src/ExperimentalExtensions';
        export { ExtensionFields } from './src/ExtensionFields';
        export { ExtensionWidgets } from './src/ExtensionWidgets';
        export { ExtensionActions } from './src/ExtensionActions';
        export * from './src/extensionEvents';
        export * from './src/launcherExtensionSlots';
        export { t, formatNumber } from './src/i18n';
        export { mount, nodes, text } from ${JSON.stringify(mock)};
      `,
      resolveDir: desktop,
      loader: "tsx",
    },
    bundle: true,
    platform: "node",
    format: "cjs",
    jsx: "automatic",
    alias: { react: mock, "react/jsx-runtime": mock, "react-dom": mock },
    loader: { ".css": "empty" },
    write: false,
  });
  modulePath = path.join(directory, "launcher.cjs");
  await writeFile(modulePath, result.outputFiles[0].text);
  compiled = require(modulePath);
});
test.afterEach(() => {
  for (const harness of mounted) harness.unmount();
  mounted.clear();
  for (const unsubscribe of subscriptions) unsubscribe();
  subscriptions.clear();
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
  invoke.operations = () => calls.map((item) => item.operation);
  return invoke;
}
function mount(Component, props) {
  const harness = compiled.mount(Component, props);
  mounted.add(harness);
  return harness;
}
const nodes = (harness) => compiled.nodes(harness.tree);
function button(harness, label) {
  const found = nodes(harness).find(
    (node) => node.type === "button" && compiled.text(node) === label,
  );
  assert.ok(found, `Missing button ${label}`);
  return found.props;
}
function child(harness, Component) {
  const found = nodes(harness).find((node) => node.type === Component);
  assert.ok(found, `Missing component ${Component.name}`);
  return found;
}
const savedWidgets = () => [
  {
    id: "notice",
    kind: "notice",
    text: "Saved launcher choices",
    tone: "info",
  },
  {
    id: "count",
    kind: "value",
    label: "Saved count",
    value: 0,
    suffix: "MiB/s",
  },
  {
    id: "progress",
    kind: "progress",
    label: "Saved progress",
    value: 0,
    max: 10,
  },
];
const card = (overrides = {}) => ({
  extensionId: "example.downloads",
  extensionName: "Downloads",
  id: "limits",
  title: "Saved limits",
  text: "Explicit controls",
  digest: "d".repeat(64),
  generation: 7,
  presentation: "compact",
  widgets: savedWidgets(),
  actions: [
    { id: "apply", label: "Apply", enabled: true, unavailableReason: null },
  ],
  ...overrides,
});
const catalogAction = () => ({
  extensionId: "example.downloads",
  extensionName: "Downloads",
  cardId: "limits",
  cardTitle: "Saved limits",
  digest: "d".repeat(64),
  generation: 7,
  id: "apply",
  label: "Apply limits",
  kind: "set-launcher-downloads",
  enabled: true,
  unavailableReason: null,
});
const props = (invoke, additions = {}) => ({
  api: invoke,
  native: true,
  slot: "settings.manage",
  onNavigate() {},
  ...additions,
});
async function cards(invoke, value = card(), additions = {}) {
  const harness = mount(compiled.ExperimentalCards, props(invoke, additions));
  const read = invoke.next("extensions_cards");
  assert.deepEqual(read.args, { slot: additions.slot ?? "settings.manage" });
  read.resolve([value]);
  await harness.flush();
  return harness;
}
async function actions(invoke) {
  const harness = mount(compiled.ExtensionActions, props(invoke));
  invoke
    .next("extensions_catalog")
    .resolve({ panels: [], actions: [catalogAction()] });
  await harness.flush();
  return harness;
}
const run = (harness, surface) =>
  button(
    harness,
    surface === "cards" ? "Apply" : compiled.t("experimental.actionRun"),
  );
function search(harness) {
  const found = nodes(harness).find(
    (node) => node.type === "input" && node.props.type === "search",
  );
  assert.ok(found, "Missing action search");
  return found.props;
}
function spy(subscribe) {
  const events = [];
  subscriptions.add(subscribe((...args) => events.push(args)));
  return events;
}

// Extract the actual arrow body rather than duplicating the main app routing
// switch in this fixture; only its state setter dependencies are substituted.
async function productionNavigation(log) {
  const source = await readFile(
    new URL("../src/main.tsx", import.meta.url),
    "utf8",
  );
  const tree = ts.createSourceFile(
      "main.tsx",
      source,
      ts.ScriptTarget.Latest,
      true,
      ts.ScriptKind.TSX,
    ),
    matches = [];
  function visit(node) {
    if (
      ts.isVariableDeclaration(node) &&
      ts.isIdentifier(node.name) &&
      node.name.text === "navigateExperimental"
    )
      matches.push(node);
    ts.forEachChild(node, visit);
  }
  visit(tree);
  assert.equal(
    matches.length,
    1,
    "Expected one production navigateExperimental closure",
  );
  assert.ok(ts.isArrowFunction(matches[0].initializer));
  const code = ts.transpileModule(
    `return (${matches[0].initializer.getText(tree)});`,
    {
      compilerOptions: {
        target: ts.ScriptTarget.ES2022,
        module: ts.ModuleKind.CommonJS,
      },
    },
  ).outputText;
  return new Function(
    "pluginSettingsDestination",
    "setSettingsPage",
    "setScreen",
    "setTab",
    code,
  )(
    compiled.pluginSettingsDestination,
    (value) => log.push(["settingsPage", value]),
    (value) => log.push(["screen", value]),
    (value) => log.push(["tab", value]),
  );
}

test("settings helpers accept only exact owned subpages and reject unknown, prototype and toolbox routes", () => {
  const mapping = {
    launch: "settings.launch",
    java: "settings.java",
    manage: "settings.manage",
    personalize: "settings.cards",
    language: "settings.language",
    misc: "settings.misc",
  };
  for (const [page, slot] of Object.entries(mapping)) {
    assert.equal(compiled.launcherSettingsPluginSlot(page), slot);
    assert.equal(compiled.pluginSettingsDestination(`settings.${page}`), page);
  }
  for (const page of [
    "",
    "toolbox",
    "tools",
    "accounts",
    "logs",
    "update",
    "__proto__",
    "constructor",
    "prototype",
    "toString",
    "hasOwnProperty",
    "manage.extra",
    "Manage",
    "manage\n",
  ]) {
    assert.equal(compiled.launcherSettingsPluginSlot(page), undefined, page);
    assert.equal(
      compiled.pluginSettingsDestination(`settings.${page}`),
      undefined,
      page,
    );
  }
  for (const target of [
    "launch",
    "tools",
    "settings",
    "settings.cards",
    "settings.manage.extra",
    "settings/manage",
  ])
    assert.equal(compiled.pluginSettingsDestination(target), undefined, target);
});

test("production main navigation selects each of the six settings subpages before screen and tab", async () => {
  const log = [],
    navigate = await productionNavigation(log);
  for (const page of [
    "launch",
    "java",
    "manage",
    "personalize",
    "language",
    "misc",
  ]) {
    log.length = 0;
    navigate(`settings.${page}`);
    assert.deepEqual(log, [
      ["settingsPage", page],
      ["screen", "home"],
      ["tab", "settings"],
    ]);
  }
  for (const target of [
    "settings.__proto__",
    "settings.constructor",
    "settings.toolboxt",
    "settings.toolBox",
    "settings.unknown",
  ]) {
    log.length = 0;
    navigate(target);
    assert.deepEqual(log, []);
  }
  log.length = 0;
  navigate("tools");
  assert.deepEqual(log, [
    ["settingsPage", "toolbox"],
    ["screen", "home"],
    ["tab", "settings"],
  ]);
  log.length = 0;
  navigate("launch");
  assert.deepEqual(log, [
    ["screen", "home"],
    ["tab", "launch"],
  ]);
});

test("widgets render plain text, localized booleans, zero, suffix and accessible saved progress without handlers or HTML", () => {
  const literal = '<img src=x onerror="throw 1">${saved}',
    widgets = [
      { id: "notice", kind: "notice", text: literal, tone: "warning" },
      { id: "true", kind: "value", label: "Enabled", value: true },
      { id: "false", kind: "value", label: "Disabled", value: false },
      { id: "zero", kind: "value", label: "Zero", value: 0, suffix: "MiB/s" },
      { id: "text", kind: "value", label: "Plain text", value: literal },
      {
        id: "progress",
        kind: "progress",
        label: "Stored completion",
        value: 0,
        max: 5,
      },
    ],
    harness = mount(compiled.ExtensionWidgets, { widgets });
  const rendered = nodes(harness),
    text = compiled.text(harness.tree);
  assert.equal(text.split(literal).length - 1, 2);
  assert.ok(text.includes(compiled.t("experimental.widgetYes")));
  assert.ok(text.includes(compiled.t("experimental.widgetNo")));
  assert.ok(text.includes(`${compiled.formatNumber(0)} MiB/s`));
  assert.ok(rendered.some((node) => node.props.role === "note"));
  for (const node of rendered) {
    assert.equal(Object.hasOwn(node.props, "dangerouslySetInnerHTML"), false);
    assert.equal(
      Object.keys(node.props).some((key) => /^on[A-Z]/.test(key)),
      false,
    );
    assert.ok(
      !["script", "img", "iframe", "a", "button", "input"].includes(node.type),
    );
  }
  const progress = rendered.find((node) => node.type === "progress");
  assert.equal(progress.props["aria-label"], "Stored completion");
  assert.equal(progress.props.value, 0);
  assert.equal(progress.props.max, 5);
});

test("editing real fields changes only drafts, keeps saved widgets and admits no automatic RPC or captured action", async () => {
  const invoke = api(),
    value = card({
      fields: [
        {
          id: "count",
          label: "Count",
          type: "number",
          default: 0,
          value: 0,
          min: 0,
          max: 10,
        },
      ],
    });
  const harness = await cards(invoke, value),
    previous = button(harness, "Apply").onClick;
  const originalWidgets = child(harness, compiled.ExtensionWidgets).props
    .widgets;
  const form = child(harness, compiled.ExtensionFields),
    tree = compiled.ExtensionFields(form.props);
  const input = compiled
    .nodes(tree)
    .find(
      (node) => node.type === "input" && node.props["aria-label"] === "Count",
    );
  assert.ok(input);
  input.props.onChange({ target: { value: "4" } });
  previous();
  assert.deepEqual(invoke.operations(), ["extensions_cards"]);
  await harness.flush();
  assert.equal(child(harness, compiled.ExtensionFields).props.draft.count, "4");
  assert.equal(
    child(harness, compiled.ExtensionWidgets).props.widgets,
    originalWidgets,
  );
  assert.deepEqual(originalWidgets, savedWidgets());
  const display = compiled.ExtensionWidgets(
    child(harness, compiled.ExtensionWidgets).props,
  );
  assert.equal(
    compiled.nodes(display).find((node) => node.type === "progress").props
      .value,
    0,
  );
  assert.equal(button(harness, "Apply").disabled, true);
  assert.equal(
    button(harness, compiled.t("experimental.settingsSave")).disabled,
    false,
  );
  assert.ok(
    nodes(harness).some((node) =>
      String(node.props.className ?? "").includes("extension-card-compact"),
    ),
  );
  assert.deepEqual(invoke.operations(), ["extensions_cards"]);
});

test("manage to java to manage retires captured buttons and pending navigation while the fresh surface remains usable", async () => {
  const invoke = api(),
    navigations = [],
    initial = props(invoke, {
      onNavigate: (target) => navigations.push(target),
    });
  const harness = await cards(invoke, card(), initial),
    stale = button(harness, "Apply").onClick;
  stale();
  const pending = invoke.next("extensions_action");
  harness.setProps({ ...initial, slot: "settings.java" });
  const java = invoke.next("extensions_cards");
  assert.deepEqual(java.args, { slot: "settings.java" });
  harness.setProps(initial);
  const manage = invoke.next("extensions_cards");
  stale();
  pending.resolve({ kind: "navigate", target: "settings.misc" });
  java.resolve([card({ title: "Retired Java card" })]);
  manage.resolve([card({ generation: 8, title: "Current management" })]);
  await harness.flush();
  stale();
  assert.deepEqual(navigations, []);
  assert.equal(invoke.count("extensions_action"), 1);
  assert.ok(!compiled.text(harness.tree).includes("Retired Java card"));
  assert.ok(compiled.text(harness.tree).includes("Current management"));
  button(harness, "Apply").onClick();
  const current = invoke.next("extensions_action");
  assert.deepEqual(current.args.expected, {
    digest: "d".repeat(64),
    generation: 8,
  });
  current.resolve({ kind: "navigate", target: "settings.java" });
  await harness.flush();
  assert.deepEqual(navigations, ["settings.java"]);
  assert.equal(harness.retiredWrites, 0);
});

test("successful download completion from cards and search broadcasts generic and appearance alias once each with feedback", async () => {
  const generic = spy(compiled.subscribeLauncherPreferencesApplied),
    legacy = spy(compiled.subscribeLauncherAppearanceApplied);
  assert.equal(
    compiled.subscribeLauncherPreferencesApplied,
    compiled.subscribeLauncherAppearanceApplied,
  );
  assert.equal(
    compiled.notifyLauncherPreferencesApplied,
    compiled.notifyLauncherAppearanceApplied,
  );
  for (const [index, surface] of ["cards", "actions"].entries()) {
    const invoke = api(),
      harness = await (surface === "cards" ? cards(invoke) : actions(invoke));
    const click = run(harness, surface).onClick;
    click();
    click();
    assert.equal(invoke.count("extensions_action"), 1);
    invoke.next("extensions_action").resolve({
      kind: "launcher-downloads-applied",
      extensionId: "example.downloads",
    });
    await harness.flush();
    assert.deepEqual(
      generic,
      Array.from({ length: index + 1 }, () => []),
    );
    assert.deepEqual(
      legacy,
      Array.from({ length: index + 1 }, () => []),
    );
    const feedback = nodes(harness)
      .filter((node) => node.props.role === "status")
      .map((node) => compiled.text(node));
    assert.ok(feedback.includes(compiled.t("experimental.downloadsApplied")));
    assert.ok(!feedback.includes(compiled.t("experimental.appearanceApplied")));
    harness.unmount();
  }
});

test("late committed downloads still broadcast after unmount or search retirement while failures never broadcast or update retired views", async () => {
  const generic = spy(compiled.subscribeLauncherPreferencesApplied),
    legacy = spy(compiled.subscribeLauncherAppearanceApplied);
  let completed = 0;
  for (const [surface, boundary] of [
    ["cards", "unmount"],
    ["actions", "unmount"],
    ["actions", "search"],
  ]) {
    for (const success of [true, false]) {
      const invoke = api(),
        harness = await (surface === "cards" ? cards(invoke) : actions(invoke));
      const stale = run(harness, surface).onClick;
      stale();
      const pending = invoke.next("extensions_action");
      if (boundary === "unmount") harness.unmount();
      else {
        search(harness).onChange({ target: { value: "no matching action" } });
        await harness.flush();
      }
      stale();
      if (success) {
        pending.resolve({
          kind: "launcher-downloads-applied",
          extensionId: "example.downloads",
        });
        completed++;
      } else pending.reject(new Error("Synthetic rejected native policy"));
      await harness.flush();
      assert.deepEqual(
        generic,
        Array.from({ length: completed }, () => []),
        `${surface}/${boundary}/${success}`,
      );
      assert.deepEqual(
        legacy,
        Array.from({ length: completed }, () => []),
      );
      assert.equal(invoke.count("extensions_action"), 1);
      assert.equal(harness.retiredWrites, 0);
      if (boundary === "search") {
        assert.ok(
          !compiled
            .text(harness.tree)
            .includes(compiled.t("experimental.downloadsApplied")),
        );
        assert.ok(
          !compiled
            .text(harness.tree)
            .includes("Synthetic rejected native policy"),
        );
      }
      harness.unmount();
    }
  }
});

test("each new launcher slot performs only its card read and never starts Agent, settings writes or actions", async () => {
  for (const slot of newSlots) {
    const invoke = api(),
      harness = await cards(invoke, card(), { slot });
    assert.deepEqual(invoke.operations(), ["extensions_cards"], slot);
    assert.equal(invoke.count("status"), 0);
    assert.equal(invoke.count("extensions_action"), 0);
    assert.equal(invoke.count("extensions_settings"), 0);
    assert.equal(button(harness, "Apply").disabled, false);
    await harness.flush();
    assert.deepEqual(invoke.operations(), ["extensions_cards"], slot);
    harness.unmount();
    const hiddenApi = api(),
      hidden = mount(
        compiled.ExperimentalCards,
        props(hiddenApi, { slot, native: false }),
      );
    await hidden.flush();
    assert.deepEqual(hiddenApi.operations(), []);
    hidden.unmount();
  }
});
