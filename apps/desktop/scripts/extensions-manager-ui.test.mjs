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
const mounted = new Set(),
  subscriptions = new Set();
let compiled, directory, modulePath;
test.before(async () => {
  const scratch = fileURLToPath(
    new URL("../../../work/plugin-system-2026-10-19/", import.meta.url),
  );
  await mkdir(scratch, { recursive: true });
  directory = await mkdtemp(path.join(scratch, "manager-ui-"));
  const result = await build({
    stdin: {
      contents: `
      export { ExperimentalExtensions, ExperimentalCards } from './src/ExperimentalExtensions';
      export { notifyExtensionsChanged, subscribeExtensionsChanged } from './src/extensionEvents';
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
    alias: { react: mock, "react/jsx-runtime": mock },
    loader: { ".css": "empty" },
    write: false,
  });
  modulePath = path.join(directory, "manager.cjs");
  await writeFile(modulePath, result.outputFiles[0].text);
  compiled = require(modulePath);
});
test.afterEach(() => {
  for (const harness of mounted) harness.unmount();
  mounted.clear();
  for (const stop of subscriptions) stop();
  subscriptions.clear();
});
test.after(async () => {
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
      (value) => value.operation === operation && !value.taken,
    );
    assert.ok(call, `Missing ${operation}`);
    call.taken = true;
    return call;
  };
  invoke.count = (operation) =>
    calls.filter((value) => value.operation === operation).length;
  invoke.calls = calls;
  return invoke;
}
const entry = (overrides = {}) => ({
  id: "example.notes",
  name: "Example notes",
  version: "1.0.0",
  publisher: "Example",
  state: "active",
  enabled: true,
  grants: ["ui.cards"],
  digest: "a".repeat(64),
  generation: 7,
  scope: "both",
  slots: ["home.secondary", "rh-code.cards"],
  ...overrides,
});
const card = (title = "Current card") => ({
  extensionId: "example.notes",
  extensionName: "Example notes",
  id: "notes",
  digest: "a".repeat(64),
  generation: 7,
  scope: "both",
  title,
  text: "Card body",
  actions: [
    { id: "go", label: "Go", enabled: true },
    { id: "summary", label: "Summary", enabled: true },
  ],
});
const review = (overrides = {}) => ({
  token: "review-once",
  id: "example.notes",
  name: "Review notes",
  version: "1.0.0",
  publisher: "Example",
  digest: "a".repeat(64),
  scope: "rh-code",
  slots: ["rh-code.cards"],
  capabilities: [
    {
      id: "ui.cards",
      required: true,
      currentlyGranted: false,
      reason: "Display cards",
    },
  ],
  ...overrides,
});
const list = (entries) => ({ entries, safeMode: false, warning: null });
const props = (invoke) => ({ api: invoke, native: true, onNavigate() {} });
function mount(Component, properties) {
  const harness = compiled.mount(Component, properties);
  mounted.add(harness);
  return harness;
}
const text = (harness) => compiled.text(harness.tree);
const nodes = (harness) => compiled.nodes(harness.tree);
const findButton = (harness, label) => {
  const found = nodes(harness).find(
    (node) => node.type === "button" && compiled.text(node) === label,
  );
  assert.ok(found, `Missing button ${label}`);
  return found.props;
};
const button = (harness, key) => findButton(harness, compiled.t(key));
const dialogs = (harness) =>
  nodes(harness).filter(
    (node) => node.type?.name === "InstanceOperationDialog",
  );
const dialog = (harness) => {
  const found = dialogs(harness)[0];
  assert.ok(found, "Missing dialog");
  return found;
};
const safeMode = (harness) =>
  nodes(harness).find((node) => node.type === "input").props;
function spyChanges() {
  const changes = [];
  subscriptions.add(
    compiled.subscribeExtensionsChanged((...args) => changes.push(args)),
  );
  return changes;
}
async function manager(invoke, entries = [entry()]) {
  const harness = mount(compiled.ExperimentalExtensions, props(invoke));
  invoke.next("extensions_list").resolve(list(entries));
  await harness.flush();
  return harness;
}
async function cards(invoke, slot = "rh-code.cards", additions = {}) {
  const harness = mount(compiled.ExperimentalCards, {
    ...props(invoke),
    slot,
    ...additions,
  });
  assert.deepEqual(invoke.next("extensions_cards").args, { slot });
  const call = invoke.calls.at(-1);
  call.resolve([card()]);
  await harness.flush();
  return harness;
}
async function importReview(harness, invoke, value = review()) {
  button(harness, "experimental.importExtension").onClick();
  invoke
    .next("experimental_choose")
    .resolve({ status: "selected", path: "fixture.json" });
  await harness.flush();
  invoke.next("extensions_review").resolve(value);
  await harness.flush();
  return dialog(harness);
}

test("enable, disable and revoke bind the displayed digest/generation and publish only after success", async () => {
  for (const [key, operation, current] of [
    [
      "experimental.communityEnable",
      "extensions_enable",
      entry({ enabled: false, state: "disabled" }),
    ],
    ["experimental.disable", "extensions_disable", entry()],
    ["experimental.revoke", "extensions_revoke", entry()],
  ]) {
    const invoke = api(),
      harness = await manager(invoke, [current]),
      changes = spyChanges();
    const saved = button(harness, key).onClick;
    saved();
    saved();
    const mutation = invoke.next(operation);
    assert.equal(invoke.count(operation), 1);
    assert.deepEqual(mutation.args, {
      id: current.id,
      expected: { digest: current.digest, generation: current.generation },
      ...(operation === "extensions_revoke" ? { capability: "ui.cards" } : {}),
    });
    assert.equal(changes.length, 0);
    mutation.resolve(entry({ generation: 8 }));
    await harness.flush();
    assert.deepEqual(changes, [[]]);
    assert.equal(invoke.count("extensions_confirm"), 0);
    invoke.next("extensions_list").resolve(list([entry({ generation: 8 })]));
    await harness.flush();
    saved();
    assert.equal(invoke.count(operation), 1);
    harness.unmount();
  }
});

test("enabling with revoked required permission leads to unchecked consent; suspended entries directly review", async () => {
  for (const state of ["disabled", "suspended"]) {
    const invoke = api(),
      current = entry({ state, enabled: state === "suspended", grants: [] });
    const harness = await manager(invoke, [current]);
    button(
      harness,
      state === "disabled"
        ? "experimental.communityEnable"
        : "experimental.communityReauthorize",
    ).onClick();
    if (state === "disabled") {
      invoke
        .next("extensions_enable")
        .resolve(entry({ state: "suspended", grants: [], generation: 8 }));
      await harness.flush();
      invoke
        .next("extensions_list")
        .resolve(
          list([entry({ state: "suspended", grants: [], generation: 8 })]),
        );
      await harness.flush();
    } else assert.equal(invoke.count("extensions_enable"), 0);
    invoke.next("extensions_rereview").resolve(review());
    await harness.flush();
    const consent = dialog(harness);
    assert.equal(consent.props.confirmDisabled, true);
    assert.equal(
      compiled.nodes(consent).find((node) => node.type === "input").props
        .checked,
      false,
    );
    assert.ok(
      compiled
        .text(consent)
        .includes(
          compiled.t("experimental.communityReviewScope", { scope: "RH Code" }),
        ),
    );
    assert.equal(invoke.count("extensions_confirm"), 0);
    harness.unmount();
  }
});

test("uninstall requires current confirmation, closes once, and sends the displayed snapshot", async () => {
  const invoke = api(),
    harness = await manager(invoke),
    changes = spyChanges();
  button(harness, "experimental.communityRemove").onClick();
  await harness.flush();
  const first = dialog(harness);
  assert.equal(invoke.count("extensions_remove"), 0);
  assert.ok(compiled.text(first).includes("Example notes"));
  first.props.onClose();
  first.props.onConfirm();
  await harness.flush();
  assert.equal(invoke.count("extensions_remove"), 0);
  button(harness, "experimental.communityRemove").onClick();
  await harness.flush();
  const second = dialog(harness);
  second.props.onConfirm();
  second.props.onConfirm();
  second.props.onClose();
  const mutation = invoke.next("extensions_remove");
  assert.deepEqual(mutation.args, {
    id: "example.notes",
    expected: { digest: "a".repeat(64), generation: 7 },
  });
  assert.equal(invoke.count("extensions_remove"), 1);
  mutation.resolve({ id: "example.notes", removed: true });
  await harness.flush();
  assert.deepEqual(changes, [[]]);
  invoke.next("extensions_list").resolve(list([]));
  await harness.flush();
  assert.equal(dialogs(harness).length, 0);
  second.props.onConfirm();
  assert.equal(invoke.count("extensions_remove"), 1);
});

test("failed uninstall consumes confirmation without publishing or refreshing; old callbacks stay retired", async () => {
  const invoke = api(),
    harness = await manager(invoke),
    changes = spyChanges();
  button(harness, "experimental.communityRemove").onClick();
  await harness.flush();
  const value = dialog(harness);
  value.props.onConfirm();
  invoke
    .next("extensions_remove")
    .reject(new Error("Fixture store write failed"));
  await harness.flush();
  assert.equal(dialogs(harness).length, 0);
  assert.match(text(harness), /Fixture store write failed/);
  assert.equal(changes.length, 0);
  assert.equal(invoke.count("extensions_list"), 1);
  value.props.onConfirm();
  assert.equal(invoke.count("extensions_remove"), 1);
});

test("uninstall confirmation retires on API/native replacement, effect restart and unmount", async () => {
  for (const replacement of ["api", "native", "effects", "unmount"]) {
    const invoke = api(),
      other = api(),
      harness = await manager(invoke);
    button(harness, "experimental.communityRemove").onClick();
    await harness.flush();
    const old = dialog(harness);
    if (replacement === "unmount") harness.unmount();
    else if (replacement === "effects") harness.restartEffects();
    else
      harness.setProps(
        replacement === "api"
          ? props(other)
          : { ...props(invoke), native: false },
      );
    old.props.onConfirm();
    old.props.onClose();
    await harness.flush();
    assert.equal(invoke.count("extensions_remove"), 0);
    assert.equal(other.count("extensions_remove"), 0);
    assert.equal(harness.retiredWrites, 0);
  }
});

test("scope filters include shared entries, default old scope to launcher, and reject hidden row callbacks", async () => {
  const invoke = api(),
    harness = await manager(invoke, [
      entry({ id: "launcher.notes", name: "Launcher only", scope: undefined }),
      entry({ id: "code.notes", name: "Code only", scope: "rh-code" }),
      entry({ id: "shared.notes", name: "Shared notes", scope: "both" }),
    ]);
  const saved = button(harness, "experimental.disable").onClick;
  findButton(harness, "RH Code").onClick();
  await harness.flush();
  assert.ok(!text(harness).includes("Launcher only"));
  assert.ok(
    text(harness).includes("Code only") &&
      text(harness).includes("Shared notes"),
  );
  saved();
  assert.equal(invoke.count("extensions_disable"), 0);
  findButton(
    harness,
    compiled.t("experimental.communityScope.launcher"),
  ).onClick();
  await harness.flush();
  assert.ok(
    text(harness).includes("Launcher only") &&
      text(harness).includes("Shared notes"),
  );
  assert.ok(!text(harness).includes("Code only"));
  assert.ok(
    nodes(harness)
      .filter((node) => node.type === "details")
      .every((node) => !node.props.open),
  );
});

test("one global event clears all slots across API wrappers, summaries and busy actions synchronously", async () => {
  const surfaces = [],
    navigations = [];
  for (const slot of ["home.secondary", "tools.cards", "rh-code.cards"]) {
    const invoke = api(),
      harness = await cards(invoke, slot, {
        onNavigate: (value) => navigations.push(value),
      });
    findButton(harness, "Summary").onClick();
    invoke.next("extensions_action").resolve({
      kind: "show-instance-summary",
      summary: {
        minecraftVersion: `OLD-${slot}`,
        loader: "Fabric",
        modCount: 1,
        isolated: true,
      },
    });
    await harness.flush();
    const saved = findButton(harness, "Go").onClick;
    saved();
    surfaces.push({
      invoke,
      harness,
      saved,
      pending: invoke.next("extensions_action"),
    });
  }
  compiled.notifyExtensionsChanged();
  for (const surface of surfaces) {
    surface.saved();
    assert.equal(surface.invoke.count("extensions_action"), 2);
    surface.pending.resolve({ kind: "navigate", target: "settings" });
    await surface.harness.flush();
    assert.ok(
      !text(surface.harness).includes("OLD-") &&
        !text(surface.harness).includes("Current card"),
    );
    surface.invoke.next("extensions_cards").resolve([card("Fresh card")]);
    await surface.harness.flush();
    assert.equal(findButton(surface.harness, "Go").disabled, false);
  }
  assert.deepEqual(navigations, []);
});

test("events retire pending card reads and late errors before a newer response can be accepted", async () => {
  for (const rejects of [false, true]) {
    const invoke = api(),
      harness = mount(compiled.ExperimentalCards, {
        ...props(invoke),
        slot: "rh-code.cards",
      });
    const old = invoke.next("extensions_cards");
    compiled.notifyExtensionsChanged();
    await harness.flush();
    invoke.next("extensions_cards").resolve([card("Newest card")]);
    await harness.flush();
    if (rejects) old.reject(new Error("Retired read failed"));
    else old.resolve([card("Retired card")]);
    await harness.flush();
    assert.ok(text(harness).includes("Newest card"));
    assert.ok(!text(harness).includes("Retired"));
    harness.unmount();
  }
});

test("failed state changes retain mounted cards; a successful late publication refreshes surviving surfaces", async () => {
  const source = api(),
    target = api(),
    harness = await manager(source),
    surface = await cards(target);
  button(harness, "experimental.disable").onClick();
  source.next("extensions_disable").reject(new Error("Fixture write refused"));
  await harness.flush();
  await surface.flush();
  assert.equal(target.count("extensions_cards"), 1);
  assert.ok(text(surface).includes("Current card"));
  button(harness, "experimental.disable").onClick();
  const pending = source.next("extensions_disable");
  harness.unmount();
  pending.resolve(entry({ state: "disabled", enabled: false }));
  await harness.flush();
  await surface.flush();
  assert.equal(target.count("extensions_cards"), 2);
  assert.ok(!text(surface).includes("Current card"));
  assert.equal(harness.retiredWrites, 0);
});

test("install confirmation and safe mode notify without granting permissions implicitly", async () => {
  const invoke = api(),
    harness = await manager(invoke),
    changes = spyChanges();
  const consent = await importReview(harness, invoke);
  const check = compiled.nodes(consent).find((node) => node.type === "input");
  check.props.onChange({ target: { checked: true } });
  consent.props.onConfirm();
  const pending = invoke.next("extensions_confirm");
  assert.deepEqual(pending.args, {
    token: "review-once",
    grants: ["ui.cards"],
  });
  pending.resolve(entry());
  await harness.flush();
  invoke.next("extensions_list").resolve(list([entry()]));
  await harness.flush();
  assert.deepEqual(changes, [[]]);
  safeMode(harness).onChange({ target: { checked: true } });
  assert.deepEqual(invoke.next("extensions_safe_mode").args, { enabled: true });
  invoke.calls.at(-1).resolve([]);
  await harness.flush();
  assert.deepEqual(changes, [[], []]);
});

test("Code cards show optional empty text only after a successful empty read and hide it during refresh/errors", async () => {
  const invoke = api(),
    harness = mount(compiled.ExperimentalCards, {
      ...props(invoke),
      slot: "rh-code.cards",
      emptyText: "No community cards",
    });
  assert.ok(!text(harness).includes("No community cards"));
  invoke.next("extensions_cards").resolve([]);
  await harness.flush();
  assert.ok(text(harness).includes("No community cards"));
  compiled.notifyExtensionsChanged();
  await harness.flush();
  assert.ok(!text(harness).includes("No community cards"));
  invoke.next("extensions_cards").reject(new Error("Fixture unavailable"));
  await harness.flush();
  assert.ok(!text(harness).includes("No community cards"));
  assert.ok(text(harness).includes("Fixture unavailable"));
});

test("card actions submit the published snapshot, while an old event owner cannot act after refresh", async () => {
  const invoke = api(),
    harness = await cards(invoke),
    saved = findButton(harness, "Go").onClick;
  saved();
  assert.deepEqual(invoke.next("extensions_action").args, {
    extensionId: "example.notes",
    cardId: "notes",
    actionId: "go",
    expected: { digest: "a".repeat(64), generation: 7 },
  });
  compiled.notifyExtensionsChanged();
  await harness.flush();
  invoke.next("extensions_cards").resolve([card("Same IDs, new owner")]);
  await harness.flush();
  saved();
  assert.equal(invoke.count("extensions_action"), 1);
  harness.unmount();
  compiled.notifyExtensionsChanged();
  assert.equal(harness.retiredWrites, 0);
});
