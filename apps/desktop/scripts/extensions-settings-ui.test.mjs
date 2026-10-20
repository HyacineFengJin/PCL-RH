import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const desktop = fileURLToPath(new URL("../", import.meta.url));
const require = createRequire(path.join(desktop, "package.json"));
const { build } = require("esbuild");
const mock = fileURLToPath(
  new URL(
    "../../../experimental/extensions/tests/fixtures/react-events.mjs",
    import.meta.url,
  ),
);
let compiled, directory, modulePath;
const mounted = new Set(),
  subscriptions = new Set();
const globals = new Map();
test.before(async () => {
  for (const [key, value] of Object.entries({
    window: { addEventListener() {}, removeEventListener() {} },
    document: { documentElement: { dataset: {}, style: { setProperty() {} } } },
    matchMedia: () => ({
      matches: false,
      addEventListener() {},
      removeEventListener() {},
    }),
    cancelAnimationFrame() {},
  })) {
    globals.set(key, Object.getOwnPropertyDescriptor(globalThis, key));
    Object.defineProperty(globalThis, key, {
      configurable: true,
      writable: true,
      value,
    });
  }
  const scratch = fileURLToPath(
    new URL("../../../work/plugin-capabilities-2026-10-20/", import.meta.url),
  );
  await mkdir(scratch, { recursive: true });
  directory = await mkdtemp(path.join(scratch, "settings-ui-"));
  const result = await build({
    stdin: {
      contents: `
        export { ExperimentalCards } from './src/ExperimentalExtensions';
        export { ExtensionFields } from './src/ExtensionFields';
        export * from './src/extensionEvents';
        export { useLauncherPreferences } from './src/useLauncherPreferences';
        export { defaultLauncherPreferenceView } from './src/launcherTypes';
        export { t } from './src/i18n';
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
  modulePath = path.join(directory, "settings.cjs");
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
  for (const [key, descriptor] of globals) {
    if (descriptor) Object.defineProperty(globalThis, key, descriptor);
    else delete globalThis[key];
  }
  if (modulePath) delete require.cache[modulePath];
  if (directory) await rm(directory, { recursive: true, force: true });
});
function api() {
  const calls = [];
  const invoke = (command, payload) =>
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
const fields = () => [
  {
    id: "title",
    label: "Title",
    type: "text",
    default: "",
    value: "Saved title",
    maxLength: 30,
  },
  {
    id: "notes",
    label: "Notes",
    type: "text",
    default: "Default notes",
    value: "Saved notes",
    maxLength: 50,
    multiline: true,
  },
  {
    id: "enabled",
    label: "Enabled",
    type: "toggle",
    default: false,
    value: true,
  },
  {
    id: "theme",
    label: "Theme",
    type: "select",
    default: "system",
    value: "light",
    options: [
      { value: "system", label: "System" },
      { value: "light", label: "Light" },
      { value: "dark", label: "Dark" },
    ],
  },
  {
    id: "count",
    label: "Count",
    type: "number",
    default: 1,
    value: 3,
    min: 1,
    max: 10,
  },
];
const card = (overrides = {}) => ({
  extensionId: "example.forms",
  extensionName: "Example forms",
  id: "form",
  title: "Form card",
  text: "Description",
  digest: "a".repeat(64),
  generation: 7,
  fields: fields(),
  actions: [{ id: "apply", label: "Apply", enabled: true }],
  ...overrides,
});
const props = (invoke, additions = {}) => ({
  api: invoke,
  native: true,
  slot: "settings.cards",
  onNavigate() {},
  ...additions,
});
async function cards(invoke, value = [card()], additions = {}) {
  const harness = mount(compiled.ExperimentalCards, props(invoke, additions));
  const read = invoke.next("extensions_cards");
  assert.deepEqual(read.args, { slot: additions.slot ?? "settings.cards" });
  read.resolve(value);
  await harness.flush();
  return harness;
}
const nodes = (harness) => compiled.nodes(harness.tree);
function button(harness, label, index = 0) {
  const found = nodes(harness).filter(
    (node) => node.type === "button" && compiled.text(node) === label,
  )[index];
  assert.ok(found, `Missing button ${label}`);
  return found.props;
}
const save = (harness, index = 0) =>
  button(harness, compiled.t("experimental.settingsSave"), index);
const reset = (harness) =>
  button(harness, compiled.t("experimental.settingsReset"));
function form(harness, index = 0) {
  const child = nodes(harness).filter(
    (node) => node.type === compiled.ExtensionFields,
  )[index];
  assert.ok(child, "Missing ExtensionFields");
  return child;
}
function control(harness, label, index = 0) {
  const tree = compiled.ExtensionFields(form(harness, index).props);
  const found = compiled
    .nodes(tree)
    .find(
      (node) =>
        node.props["aria-label"] === label ||
        (label === "Enabled" &&
          node.type === "label" &&
          compiled.text(node) === label),
    );
  assert.ok(found, `Missing ${label}`);
  if (label === "Enabled")
    return compiled.nodes(found).find((node) => node.type === "input");
  return found;
}
function change(harness, label, value, index = 0) {
  const input = control(harness, label, index);
  input.props.onChange({
    target: typeof value === "boolean" ? { checked: value } : { value },
  });
}
function spy(subscribe) {
  const events = [];
  subscriptions.add(subscribe((...args) => events.push(args)));
  return events;
}

test("trusted fields render text, textarea, checkbox, CE select and bounded integer controls", async () => {
  const invoke = api(),
    harness = await cards(invoke);
  assert.equal(control(harness, "Title").props.type, "text");
  assert.equal(control(harness, "Notes").type, "textarea");
  assert.equal(control(harness, "Enabled").props.type, "checkbox");
  assert.equal(control(harness, "Theme").type.name, "CeSelect");
  assert.equal(control(harness, "Count").props.type, "number");
  assert.equal(control(harness, "Count").props.min, 1);
  assert.equal(control(harness, "Count").props.max, 10);
  assert.equal(control(harness, "Title").props.maxLength, 60);
  assert.equal(save(harness).disabled, true);
  assert.equal(button(harness, "Apply").disabled, false);
});

test("edits have no IO, retire saved action callbacks synchronously and explicitly save partial typed values", async () => {
  const invoke = api(),
    harness = await cards(invoke),
    oldAction = button(harness, "Apply").onClick;
  change(harness, "Title", "<script>plain text</script>");
  oldAction();
  assert.equal(invoke.count("extensions_action"), 0);
  change(harness, "Enabled", false);
  change(harness, "Theme", "dark");
  change(harness, "Count", "5");
  assert.equal(invoke.count("extensions_settings"), 0);
  await harness.flush();
  assert.equal(button(harness, "Apply").disabled, true);
  assert.equal(save(harness).disabled, false);
  const submit = save(harness).onClick;
  submit();
  submit();
  const write = invoke.next("extensions_settings");
  assert.equal(invoke.count("extensions_settings"), 1);
  assert.deepEqual(write.args, {
    id: "example.forms",
    expected: { digest: "a".repeat(64), generation: 7 },
    values: {
      title: "<script>plain text</script>",
      enabled: false,
      theme: "dark",
      count: 5,
    },
  });
  write.resolve({ generation: 8 });
  await harness.flush();
  assert.equal(invoke.count("extensions_cards"), 2);
});

test("reset changes only drafts; failure retains every draft and retry can save empty text", async () => {
  const invoke = api(),
    harness = await cards(invoke),
    events = spy(compiled.subscribeExtensionsChanged);
  reset(harness).onClick();
  await harness.flush();
  assert.deepEqual(form(harness).props.draft, {
    title: "",
    notes: "Default notes",
    enabled: false,
    theme: "system",
    count: 1,
  });
  assert.equal(invoke.count("extensions_settings"), 0);
  save(harness).onClick();
  invoke
    .next("extensions_settings")
    .reject(new Error("Synthetic durable failure"));
  await harness.flush();
  assert.deepEqual(events, []);
  assert.equal(control(harness, "Notes").props.value, "Default notes");
  assert.equal(control(harness, "Title").props.value, "");
  assert.equal(control(harness, "Enabled").props.checked, false);
  assert.equal(control(harness, "Count").props.value, "1");
  assert.equal(save(harness).disabled, false);
  assert.ok(compiled.text(harness.tree).includes("Synthetic durable failure"));
  save(harness).onClick();
  assert.equal(invoke.next("extensions_settings").args.values.title, "");
});

test("blank, fractional and out-of-range numbers and overlong text block even an earlier save callback", async () => {
  const invoke = api(),
    harness = await cards(invoke);
  change(harness, "Title", "Changed");
  await harness.flush();
  const submit = save(harness).onClick;
  for (const value of ["", "2.5", "11", "-1", "Infinity"]) {
    change(harness, "Count", value);
    submit();
    await harness.flush();
    assert.equal(save(harness).disabled, true);
    assert.equal(invoke.count("extensions_settings"), 0);
  }
  change(harness, "Count", "3");
  change(harness, "Title", "x".repeat(31));
  submit();
  await harness.flush();
  assert.equal(save(harness).disabled, true);
  assert.equal(invoke.count("extensions_settings"), 0);
});

test("text settings count Unicode code points and enforce the host control-character boundary", async () => {
  const invoke = api(),
    definitions = fields();
  definitions[0] = { ...definitions[0], maxLength: 2, value: "", default: "" };
  const harness = await cards(invoke, [card({ fields: definitions })]);
  assert.equal(control(harness, "Title").props.maxLength, 4);
  change(harness, "Title", "😀😀");
  change(harness, "Notes", "😀\n\tText");
  await harness.flush();
  assert.equal(save(harness).disabled, false);
  const submit = save(harness).onClick;
  for (const value of [
    "😀😀😀",
    "a\n",
    "a\t",
    "a\0",
    "a\r",
    "a\u007f",
    "a\u0085",
    "a\u202e",
    "a\ud800",
  ]) {
    change(harness, "Title", value);
    submit();
    await harness.flush();
    assert.equal(
      save(harness).disabled,
      true,
      `Accepted unsafe text ${JSON.stringify(value)}`,
    );
    assert.equal(invoke.count("extensions_settings"), 0);
  }
  change(harness, "Title", "😀😀");
  change(harness, "Notes", "Text\u200e");
  await harness.flush();
  assert.equal(save(harness).disabled, true);
  change(harness, "Notes", "😀\n\tText");
  await harness.flush();
  save(harness).onClick();
  assert.deepEqual(invoke.next("extensions_settings").args.values, {
    title: "😀😀",
    notes: "😀\n\tText",
  });
});

test("saving one card and unrelated publications retain another card's dirty fields while adopting clean fields", async () => {
  const invoke = api(),
    first = card(),
    second = card({
      extensionId: "example.other",
      id: "notes",
      title: "Other notes",
    });
  const harness = await cards(invoke, [first, second]);
  const oldAction = button(harness, "Apply", 1).onClick;
  change(harness, "Title", "Saved A");
  change(harness, "Notes", "Unsaved B", 1);
  await harness.flush();
  save(harness).onClick();
  invoke.next("extensions_settings").resolve({ generation: 8 });
  await harness.flush();
  const savedFirst = card({
    generation: 8,
    fields: fields().map((field) =>
      field.id === "title" ? { ...field, value: "Saved A" } : field,
    ),
  });
  invoke.next("extensions_cards").resolve([savedFirst, card({ ...second })]);
  await harness.flush();
  assert.equal(control(harness, "Title").props.value, "Saved A");
  assert.equal(save(harness).disabled, true);
  assert.equal(button(harness, "Apply").disabled, false);
  assert.equal(control(harness, "Notes", 1).props.value, "Unsaved B");
  assert.equal(save(harness, 1).disabled, false);
  assert.equal(button(harness, "Apply", 1).disabled, true);
  change(harness, "Count", "", 1);
  compiled.notifyExtensionsChanged(); // e.g. another plugin was uninstalled.
  await harness.flush();
  invoke.next("extensions_cards").resolve([
    savedFirst,
    card({
      ...second,
      generation: 8,
      fields: fields().map((field) =>
        field.id === "enabled" ? { ...field, value: false } : field,
      ),
    }),
  ]);
  await harness.flush();
  assert.equal(control(harness, "Notes", 1).props.value, "Unsaved B");
  assert.equal(control(harness, "Count", 1).props.value, "");
  assert.equal(save(harness, 1).disabled, true);
  assert.equal(control(harness, "Enabled", 1).props.checked, false);
  oldAction();
  assert.equal(invoke.count("extensions_action"), 0);
});

test("draft caches evict missing cards, changed manifests or definitions and API/native/slot surfaces", async () => {
  for (const kind of [
    "missing",
    "manifest",
    "definition",
    "api",
    "hide",
    "slot",
  ]) {
    const invoke = api(),
      other = api(),
      harness = await cards(invoke);
    change(harness, "Notes", "Must not revive");
    await harness.flush();
    const oldFields = form(harness).props.onChange,
      oldSubmit = save(harness).onClick;
    let target = invoke;
    if (["missing", "manifest", "definition"].includes(kind)) {
      compiled.notifyExtensionsChanged();
      await harness.flush();
      if (kind === "missing") {
        invoke.next("extensions_cards").resolve([]);
        await harness.flush();
        compiled.notifyExtensionsChanged();
        await harness.flush();
      }
    }
    if (kind === "api") {
      harness.setProps(props(other));
      target = other;
    }
    if (kind === "hide") {
      harness.setProps(props(invoke, { native: false }));
      harness.setProps(props(invoke));
    }
    if (kind === "slot")
      harness.setProps(props(invoke, { slot: "home.secondary" }));
    const fresh =
      kind === "manifest"
        ? card({ digest: "b".repeat(64) })
        : kind === "definition"
          ? card({
              fields: fields().map((field) =>
                field.id === "notes" ? { ...field, maxLength: 40 } : field,
              ),
            })
          : card();
    target.next("extensions_cards").resolve([fresh]);
    await harness.flush();
    assert.equal(control(harness, "Notes").props.value, "Saved notes", kind);
    assert.equal(save(harness).disabled, true, kind);
    oldFields("notes", "Retired callback");
    oldSubmit();
    assert.equal(invoke.count("extensions_settings"), 0, kind);
    assert.equal(other.count("extensions_settings"), 0, kind);
    harness.unmount();
  }
});

test("local action completion feedback clears on field changes and publication refresh", async () => {
  for (const [kind, message] of [
    ["text-copied", "experimental.textCopied"],
    ["launcher-appearance-applied", "experimental.appearanceApplied"],
  ]) {
    const invoke = api(),
      harness = await cards(invoke);
    button(harness, "Apply").onClick();
    invoke
      .next("extensions_action")
      .resolve({ kind, extensionId: "example.forms" });
    await harness.flush();
    assert.ok(compiled.text(harness.tree).includes(compiled.t(message)));
    change(harness, "Notes", "Draft");
    await harness.flush();
    assert.ok(!compiled.text(harness.tree).includes(compiled.t(message)));
    change(harness, "Notes", "Saved notes");
    await harness.flush();
    button(harness, "Apply").onClick();
    invoke
      .next("extensions_action")
      .resolve({ kind, extensionId: "example.forms" });
    await harness.flush();
    compiled.notifyExtensionsChanged();
    await harness.flush();
    assert.ok(!compiled.text(harness.tree).includes(compiled.t(message)));
    harness.unmount();
  }
});

test("saving retires sibling cards, pending actions and old callbacks across API wrappers", async () => {
  const first = api(),
    other = api(),
    navigations = [];
  const source = await cards(first),
    sibling = await cards(other, [card({ id: "sibling", fields: undefined })], {
      slot: "rh-code.cards",
      onNavigate: (value) => navigations.push(value),
    });
  const oldAction = button(sibling, "Apply").onClick;
  oldAction();
  const pending = other.next("extensions_action");
  change(source, "Notes", "Updated");
  await source.flush();
  const oldFields = form(source).props.onChange,
    oldReset = reset(source).onClick;
  save(source).onClick();
  first.next("extensions_settings").resolve({ generation: 8 });
  await source.flush();
  await sibling.flush();
  oldAction();
  oldFields("notes", "Retired");
  oldReset();
  pending.resolve({ kind: "navigate", target: "settings" });
  await sibling.flush();
  assert.deepEqual(navigations, []);
  assert.equal(other.count("extensions_action"), 1);
  other
    .next("extensions_cards")
    .resolve([card({ id: "sibling", fields: undefined, generation: 8 })]);
  first.next("extensions_cards").resolve([card({ generation: 8 })]);
  await source.flush();
  await sibling.flush();
  oldAction();
  assert.equal(other.count("extensions_action"), 1);
  assert.equal(control(source, "Notes").props.value, "Saved notes");
});

test("successful late save refreshes surviving cards after its origin unmounts", async () => {
  const first = api(),
    second = api(),
    source = await cards(first),
    sibling = await cards(second);
  change(source, "Notes", "Late save");
  await source.flush();
  save(source).onClick();
  const pending = first.next("extensions_settings");
  source.unmount();
  pending.resolve({ generation: 8 });
  await source.flush();
  await sibling.flush();
  assert.equal(second.count("extensions_cards"), 2);
  assert.equal(source.retiredWrites, 0);
});

test("native hide and return, API replacement, slot replacement and effect restart retire form callbacks", async () => {
  for (const kind of ["hide", "api", "slot", "effects", "unmount"]) {
    const invoke = api(),
      harness = await cards(invoke),
      other = api();
    change(harness, "Notes", "Unsaved");
    await harness.flush();
    const submit = save(harness).onClick,
      oldReset = reset(harness).onClick,
      update = form(harness).props.onChange;
    if (kind === "hide") {
      harness.setProps(props(invoke, { native: false }));
      harness.setProps(props(invoke));
    }
    if (kind === "api") harness.setProps(props(other));
    if (kind === "slot")
      harness.setProps(props(invoke, { slot: "home.secondary" }));
    if (kind === "effects") harness.restartEffects();
    if (kind === "unmount") harness.unmount();
    submit();
    oldReset();
    update("notes", "Retired");
    assert.equal(invoke.count("extensions_settings"), 0);
    assert.equal(other.count("extensions_settings"), 0);
    assert.equal(harness.retiredWrites, 0);
    harness.unmount();
  }
});

test("only a committed appearance intent publishes a payload-free preference notification, including a late completion", async () => {
  const events = spy(compiled.subscribeLauncherAppearanceApplied);
  for (const kind of ["text-copied", "launcher-appearance-applied"]) {
    const invoke = api(),
      harness = await cards(invoke, [card({ fields: undefined })]);
    button(harness, "Apply").onClick();
    const pending = invoke.next("extensions_action");
    if (kind === "launcher-appearance-applied") harness.unmount();
    pending.resolve({ kind, extensionId: "example.forms" });
    await harness.flush();
  }
  assert.deepEqual(events, [[]]);
});

function preferenceView(revision, theme = "light") {
  const value = compiled.defaultLauncherPreferenceView();
  value.revision = revision;
  value.preferences.appearance.theme = theme;
  return value;
}
function PreferenceProbe({ invoke, native = true, notify = () => {} }) {
  return compiled.useLauncherPreferences(invoke, native, notify);
}
async function preferences(invoke, initial = preferenceView("initial")) {
  const harness = mount(PreferenceProbe, { invoke });
  invoke.next("launcher_preferences").resolve(initial);
  await harness.flush();
  return harness;
}

test("appearance event rereads real preferences and applies their existing runtime effects", async () => {
  const invoke = api(),
    harness = await preferences(invoke);
  compiled.notifyLauncherAppearanceApplied();
  const read = invoke.next("launcher_preferences"),
    latest = preferenceView("actual", "dark");
  latest.preferences.appearance.custom_cursor = true;
  latest.preferences.navigation.hidden_menu_ids = ["settings.misc"];
  read.resolve(latest);
  await harness.flush();
  assert.equal(harness.tree.view.revision, "actual");
  assert.equal(
    globalThis.document.documentElement.dataset.launcherTheme,
    "dark",
  );
  assert.equal(
    globalThis.document.documentElement.dataset.customCursor,
    "true",
  );
  assert.deepEqual(harness.tree.prefs.navigation.hidden_menu_ids, [
    "settings.misc",
  ]);
});

test("an appearance refresh queued before a local edit cannot overwrite that edit", async () => {
  const invoke = api(),
    harness = await preferences(invoke);
  compiled.notifyLauncherAppearanceApplied();
  const oldRead = invoke.next("launcher_preferences");
  const result = harness.tree.patch({ appearance: { theme: "dark" } });
  invoke
    .next("launcher_preferences_update")
    .resolve(preferenceView("local-edit", "dark"));
  await result;
  await harness.flush();
  assert.equal(harness.tree.view.revision, "local-edit");
  oldRead.resolve(preferenceView("old-read", "light"));
  await harness.flush();
  assert.equal(harness.tree.view.revision, "local-edit");
  invoke.next("launcher_preferences").resolve(preferenceView("latest", "dark"));
  await harness.flush();
  assert.equal(harness.tree.view.revision, "latest");
});

test("appearance publication during a pending patch rejects its late full snapshot and rereads after completion", async () => {
  const invoke = api(),
    harness = await preferences(invoke);
  const result = harness.tree.patch({ appearance: { theme: "dark" } });
  const pending = invoke.next("launcher_preferences_update");
  compiled.notifyLauncherAppearanceApplied();
  assert.equal(invoke.count("launcher_preferences"), 1);
  pending.resolve(preferenceView("stale-patch", "light"));
  await result;
  await harness.flush();
  assert.equal(harness.tree.view.revision, "initial");
  invoke
    .next("launcher_preferences")
    .resolve(preferenceView("committed-plugin", "dark"));
  await harness.flush();
  assert.equal(harness.tree.view.revision, "committed-plugin");
  assert.equal(harness.tree.busy, false);
});

test("repeated appearance events invalidate an in-flight read and coalesce a fresh read", async () => {
  const invoke = api(),
    harness = await preferences(invoke);
  compiled.notifyLauncherAppearanceApplied();
  const old = invoke.next("launcher_preferences");
  compiled.notifyLauncherAppearanceApplied();
  compiled.notifyLauncherAppearanceApplied();
  old.resolve(preferenceView("retired", "dark"));
  await harness.flush();
  assert.equal(harness.tree.view.revision, "initial");
  assert.equal(invoke.count("launcher_preferences"), 3);
  invoke.next("launcher_preferences").resolve(preferenceView("fresh", "dark"));
  await harness.flush();
  assert.equal(harness.tree.view.revision, "fresh");
});

test("preference API/native lifetimes reject old patch/read completions and unmount performs no state writes", async () => {
  for (const kind of ["api", "hide", "unmount"]) {
    const invoke = api(),
      other = api(),
      harness = await preferences(invoke);
    const result = harness.tree.patch({ appearance: { theme: "dark" } });
    const pending = invoke.next("launcher_preferences_update");
    if (kind === "api") harness.setProps({ invoke: other });
    if (kind === "hide") {
      harness.setProps({ invoke, native: false });
      harness.setProps({ invoke });
    }
    if (kind === "unmount") harness.unmount();
    pending.resolve(preferenceView("old-patch", "dark"));
    await result;
    await harness.flush();
    if (kind !== "unmount") {
      const target = kind === "api" ? other : invoke;
      target
        .next("launcher_preferences")
        .resolve(preferenceView("new-owner", "light"));
      await harness.flush();
      assert.equal(harness.tree.view.revision, "new-owner");
      assert.equal(harness.tree.busy, false);
    }
    assert.equal(harness.retiredWrites, 0);
    harness.unmount();
  }
});

test("effect restarts retire initial preference reads and arrange a replacement read", async () => {
  const invoke = api(),
    harness = mount(PreferenceProbe, { invoke });
  const old = invoke.next("launcher_preferences");
  harness.restartEffects();
  old.resolve(preferenceView("retired-initial", "dark"));
  await harness.flush();
  assert.notEqual(harness.tree.view.revision, "retired-initial");
  invoke
    .next("launcher_preferences")
    .resolve(preferenceView("new-lifetime", "light"));
  await harness.flush();
  assert.equal(harness.tree.view.revision, "new-lifetime");
});

test("failed preference refresh retains current state and a subsequent appearance event retries", async () => {
  const invoke = api(),
    notices = [],
    harness = await preferences(invoke);
  harness.setProps({ invoke, notify: (value) => notices.push(value) });
  compiled.notifyLauncherAppearanceApplied();
  invoke
    .next("launcher_preferences")
    .reject(new Error("Synthetic preference read failure"));
  await harness.flush();
  assert.equal(harness.tree.view.revision, "initial");
  assert.equal(notices.length, 1);
  compiled.notifyLauncherAppearanceApplied();
  invoke.next("launcher_preferences").resolve(preferenceView("retry", "dark"));
  await harness.flush();
  assert.equal(harness.tree.view.revision, "retry");
});

test("a failed patch reconciles the current revision before a queued appearance refresh", async () => {
  const invoke = api(),
    harness = await preferences(invoke);
  const result = harness.tree.patch({ appearance: { theme: "dark" } });
  const rejected = assert.rejects(result, /Synthetic patch failure/);
  compiled.notifyLauncherAppearanceApplied();
  invoke
    .next("launcher_preferences_update")
    .reject(new Error("Synthetic patch failure"));
  await harness.flush();
  invoke
    .next("launcher_preferences")
    .resolve(preferenceView("intermediate", "light"));
  await rejected;
  await harness.flush();
  assert.equal(harness.tree.view.revision, "initial");
  assert.equal(harness.tree.busy, false);
  invoke.next("launcher_preferences").resolve(preferenceView("final", "dark"));
  await harness.flush();
  assert.equal(harness.tree.view.revision, "final");
});
