/** Production memory-card events with synthetic native replies and saves. */
import assert from "node:assert/strict";
import fs from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";
import {
  harness,
  elements,
  content,
  button,
  deferred,
} from "./maker-test-harness.mjs";

const base = Object.freeze({
  root_id: "root-a",
  root: "/synthetic/game",
  revision: "settings-one",
  player: "Player",
  memory_gib: 8,
  selected: "example",
  overrides: Object.freeze({}),
});
const info = {
  total_memory_bytes: 32 * 1073741824,
  available_memory_bytes: 16 * 1073741824,
};
function preview(settings = base, instanceId = null, extra = {}) {
  return {
    root_id: settings.root_id,
    instance_id: instanceId,
    settings_revision: settings.revision,
    mode: "auto",
    memory_gib: 5,
    manual_gib: settings.memory_gib,
    total_memory_bytes: info.total_memory_bytes,
    available_memory_bytes: info.available_memory_bytes,
    mod_count: instanceId === null ? null : 41,
    constrained: false,
    error: null,
    ...extra,
  };
}
function view(extra = {}, options = {}) {
  const saved = [],
    notices = [],
    calls = [];
  let ui;
  const api = async (command, args) => {
    calls.push({ command, args });
    if (command === "system_info") return info;
    assert.equal(command, "memory_preview");
    return options.read
      ? options.read(args)
      : preview(ui?.savedSettings || extra.settings || base, args.id);
  };
  ui = harness("MemoryControl", {
    settings: base,
    api,
    native: true,
    disabled: false,
    onSave: async (settings) => {
      saved.push(settings);
      await options.save?.(settings);
    },
    onNotify: (message) => notices.push(message),
    ...extra,
  });
  return { ui, saved, notices, calls };
}
function radio(ui, choice) {
  const result = elements(
    ui.tree,
    (node) =>
      node.type === "input" &&
      node.props.type === "radio" &&
      node.props.value === choice,
  )[0];
  assert.ok(result, `Missing ${choice} radio`);
  return result;
}
function slider(ui) {
  return elements(
    ui.tree,
    (node) => node.type === "input" && node.props.type === "range",
  )[0];
}
function allocation(ui) {
  const values = elements(
    ui.tree,
    (node) => node.props?.className === "ce-memory-values",
  )[0];
  return content(values.props.children[1]);
}
async function choose(ui, choice) {
  radio(ui, choice).props.onChange();
  await ui.flush();
}
async function drag(ui, value) {
  slider(ui).props.onChange({ target: { value: String(value) } });
  await ui.flush();
}
async function accept(ui, settings, revision = "settings-two") {
  ui.savedSettings = { ...settings, revision };
  ui.replace({ settings: ui.savedSettings });
  await ui.flush();
}

test("global Auto saves its policy, preserves manual GiB, and Custom restores it", async () => {
  const { ui, saved, calls } = view();
  await ui.flush();
  assert.equal(
    calls.some((call) => call.command === "memory_preview"),
    false,
  );
  assert.equal(radio(ui, "custom").props.checked, true);
  await choose(ui, "auto");
  assert.equal(saved[0].memory_mode, "auto");
  assert.equal(saved[0].memory_gib, 8);
  assert.equal(
    radio(ui, "auto").props.checked,
    false,
    "only confirmed settings select the new mode",
  );
  await accept(ui, saved[0]);
  assert.equal(radio(ui, "auto").props.checked, true);
  assert.equal(slider(ui).props.disabled, true);
  assert.match(allocation(ui), /^5\.0 GiB/);
  assert.deepEqual(
    calls.find((call) => call.command === "memory_preview").args,
    { id: null },
  );
  assert.match(content(ui.tree), /启动前重新计算/);
  await choose(ui, "custom");
  assert.equal(saved[1].memory_mode, "manual");
  assert.equal(saved[1].memory_gib, 8);
  await accept(ui, saved[1], "settings-three");
  assert.equal(slider(ui).props.value, 8);
  assert.equal(slider(ui).props.disabled, false);
  ui.close();
});

test("manual draft survives unrelated revision while the earlier event is retired", async () => {
  const { ui, saved } = view();
  await drag(ui, 11);
  const stale = slider(ui).props.onPointerUp;
  await accept(ui, { ...base, player: "Other" });
  assert.equal(slider(ui).props.value, 11);
  stale();
  await ui.flush();
  assert.deepEqual(saved, []);
  slider(ui).props.onPointerUp();
  await ui.flush();
  assert.equal(saved[0].memory_gib, 11);
  assert.equal(saved[0].player, "Other");
  assert.equal(saved[0].revision, "settings-two");
  ui.close();
});

test("Auto keeps an unsaved manual draft for a later Custom choice", async () => {
  const { ui, saved } = view();
  await drag(ui, 10);
  await choose(ui, "auto");
  assert.equal(saved[0].memory_gib, 8);
  await accept(ui, saved[0]);
  assert.equal(slider(ui).props.value, 10);
  await choose(ui, "custom");
  assert.equal(saved[1].memory_gib, 10);
  ui.close();
});

test("failed mode and slider saves keep saved selection and editable draft", async () => {
  const { ui, saved, notices } = view(
    {},
    {
      save: async () => {
        throw new Error("synthetic save rejected");
      },
    },
  );
  await choose(ui, "auto");
  assert.equal(radio(ui, "custom").props.checked, true);
  assert.equal(radio(ui, "auto").props.checked, false);
  assert.match(notices[0], /save rejected/);
  await drag(ui, 9);
  slider(ui).props.onBlur();
  await ui.flush();
  assert.equal(saved[1].memory_gib, 9);
  assert.equal(slider(ui).props.value, 9);
  assert.equal(slider(ui).props.disabled, false);
  assert.equal(ui.props.settings.memory_gib, 8);
  assert.equal(notices.length, 2);
  ui.close();
});

test("instance legacy manual override wins over global Auto and Auto preserves that override", async () => {
  const settings = {
    ...base,
    memory_mode: "auto",
    overrides: { example: 7, other: 6 },
    memory_mode_overrides: { other: "auto" },
  };
  const { ui, saved } = view({ settings, instanceId: "example" });
  await ui.flush();
  assert.equal(radio(ui, "custom").props.checked, true);
  assert.equal(slider(ui).props.value, 7);
  await choose(ui, "auto");
  assert.deepEqual(saved[0].overrides, settings.overrides);
  assert.deepEqual(saved[0].memory_mode_overrides, {
    example: "auto",
    other: "auto",
  });
  await accept(ui, saved[0]);
  await choose(ui, "custom");
  assert.equal(saved[1].memory_mode_overrides.example, "manual");
  assert.equal(saved[1].overrides.example, 7);
  assert.equal(saved[1].overrides.other, 6);
  ui.close();
});

test("following global Auto requests the actual instance estimate; global removes both instance maps", async () => {
  const settings = {
    ...base,
    memory_mode: "auto",
    overrides: { example: 9, other: 6 },
    memory_mode_overrides: { example: "auto", other: "manual" },
  };
  const { ui, saved, calls } = view({ settings, instanceId: "example" });
  await ui.flush();
  await choose(ui, "global");
  assert.equal(Object.hasOwn(saved[0].overrides, "example"), false);
  assert.equal(Object.hasOwn(saved[0].memory_mode_overrides, "example"), false);
  assert.equal(saved[0].overrides.other, 6);
  assert.equal(saved[0].memory_mode_overrides.other, "manual");
  await accept(ui, saved[0]);
  assert.equal(radio(ui, "global").props.checked, true);
  assert.equal(radio(ui, "auto").props.checked, false);
  assert.equal(slider(ui).props.disabled, true);
  assert.match(allocation(ui), /^5\.0 GiB/);
  assert.ok(
    calls
      .filter((call) => call.command === "memory_preview")
      .every((call) => call.args.id === "example"),
  );
  ui.close();
});

test("inherited manual allocation follows global GiB; explicit manual can omit its numeric override", async () => {
  const { ui, saved } = view({ instanceId: "example" });
  await ui.flush();
  assert.equal(radio(ui, "global").props.checked, true);
  assert.match(allocation(ui), /^8\.0 GiB/);
  await accept(ui, {
    ...base,
    memory_gib: 6,
    memory_mode_overrides: { example: "manual" },
  });
  assert.equal(radio(ui, "custom").props.checked, true);
  assert.equal(slider(ui).props.value, 6);
  await drag(ui, 9);
  slider(ui).props.onKeyUp();
  await ui.flush();
  assert.equal(saved[0].overrides.example, 9);
  assert.equal(saved[0].memory_mode_overrides.example, "manual");
  assert.equal(saved[0].memory_gib, 6);
  ui.close();
});

test("valid __proto__ instance ID remains an own key through all three choices", async () => {
  const { ui, saved } = view({ instanceId: "__proto__" });
  assert.equal(radio(ui, "global").props.checked, true);
  await choose(ui, "custom");
  assert.equal(Object.hasOwn(saved[0].overrides, "__proto__"), true);
  assert.equal(saved[0].overrides.__proto__, 8);
  assert.equal(saved[0].memory_mode_overrides.__proto__, "manual");
  await accept(ui, saved[0]);
  await choose(ui, "auto");
  assert.equal(
    JSON.parse(JSON.stringify(saved[1])).memory_mode_overrides.__proto__,
    "auto",
  );
  await accept(ui, saved[1], "settings-three");
  await choose(ui, "global");
  assert.equal(Object.hasOwn(saved[2].overrides, "__proto__"), false);
  assert.equal(
    Object.hasOwn(saved[2].memory_mode_overrides, "__proto__"),
    false,
  );
  ui.close();
});

test("preview read failure and native error never fabricate an allocation and allow retry", async () => {
  let attempt = 0;
  const settings = { ...base, memory_mode: "auto" };
  const { ui, calls } = view(
    { settings },
    {
      read: async () => {
        attempt++;
        if (attempt === 1) throw new Error("synthetic probe failure");
        return preview(
          settings,
          null,
          attempt === 2
            ? { memory_gib: 8, error: "synthetic native unavailable" }
            : {},
        );
      },
    },
  );
  await ui.flush();
  assert.match(content(ui.tree), /synthetic probe failure/);
  assert.match(allocation(ui), /^—/);
  button(ui, "memory.refresh").props.onClick();
  await ui.flush();
  assert.match(content(ui.tree), /synthetic native unavailable/);
  assert.match(allocation(ui), /^—/);
  button(ui, "memory.refresh").props.onClick();
  await ui.flush();
  assert.match(allocation(ui), /^5\.0 GiB/);
  assert.equal(
    calls.filter((call) => call.command === "memory_preview").length,
    3,
  );
  ui.close();
});

for (const mismatch of [
  { root_id: "root-b" },
  { instance_id: "other" },
  { settings_revision: "stale" },
  { mode: "manual" },
  { memory_gib: 0 },
])
  test(`preview rejects a response with ${JSON.stringify(mismatch)}`, async () => {
    const settings = { ...base, memory_mode: "auto" };
    const { ui } = view(
      { settings },
      { read: async () => preview(settings, null, mismatch) },
    );
    await ui.flush();
    assert.match(content(ui.tree), /设置已变化/);
    assert.match(allocation(ui), /^—/);
    ui.close();
  });

test("late preview replies cannot cross root, instance, revision or disabled round trips", async () => {
  const pending = [],
    settings = { ...base, memory_mode: "auto" };
  const { ui } = view(
    { settings, instanceId: "example" },
    {
      read: () => {
        const request = deferred();
        pending.push(request);
        return request.promise;
      },
    },
  );
  await ui.flush();
  const staleRefresh = button(ui, "memory.refresh").props.onClick;
  ui.replace({
    settings: { ...settings, root_id: "root-b", root: "/synthetic/other" },
  });
  await ui.flush();
  ui.replace({ settings, instanceId: "other" });
  await ui.flush();
  ui.replace({
    instanceId: "example",
    settings: { ...settings, revision: "settings-two" },
  });
  await ui.flush();
  ui.replace({ disabled: true });
  await ui.flush();
  ui.replace({ disabled: false, settings });
  await ui.flush();
  assert.equal(pending.length, 5);
  staleRefresh();
  await ui.flush();
  assert.equal(pending.length, 5);
  pending.at(-1).resolve(preview(settings, "example", { memory_gib: 6 }));
  await ui.flush();
  for (const old of pending.slice(0, -1))
    old.resolve(preview(settings, "example", { memory_gib: 13 }));
  await ui.flush();
  assert.match(allocation(ui), /^6\.0 GiB/);
  assert.equal(
    elements(ui.tree, (node) => node.props?.role === "alert").length,
    0,
  );
  ui.close();
});

test("old save events and late failures retire on root, instance, API and disabled changes", async () => {
  for (const change of [
    { settings: { ...base, root_id: "root-b" } },
    { instanceId: "other" },
    { api: async () => info },
    { disabled: true },
  ]) {
    const pending = deferred();
    const { ui, saved, notices } = view(
      { instanceId: "example" },
      { save: () => pending.promise },
    );
    const stale = radio(ui, "auto").props.onChange;
    stale();
    stale();
    await ui.flush();
    assert.equal(saved.length, 1);
    ui.replace(change);
    await ui.flush();
    pending.reject(new Error("retired failure"));
    await ui.flush();
    assert.deepEqual(notices, []);
    ui.replace({ settings: base, instanceId: "example", disabled: false });
    await ui.flush();
    stale();
    await ui.flush();
    assert.equal(saved.length, 1);
    ui.close();
  }
});

test("disabled and preview-only controls reject synthetic old events; unmount ignores late replies", async () => {
  for (const extra of [{ native: false }, { disabled: true }]) {
    const { ui, saved, calls } = view({
      ...extra,
      settings: { ...base, memory_mode: "auto" },
    });
    await ui.flush();
    assert.equal(radio(ui, "auto").props.disabled, true);
    assert.equal(slider(ui).props.disabled, true);
    await choose(ui, "custom");
    await drag(ui, 12);
    slider(ui).props.onPointerUp();
    button(ui, "memory.refresh").props.onClick();
    await ui.flush();
    assert.deepEqual(saved, []);
    assert.equal(
      calls.some((call) => call.command === "memory_preview"),
      false,
    );
    ui.close();
  }
  const pending = deferred();
  const { ui } = view(
    { settings: { ...base, memory_mode: "auto" } },
    { read: () => pending.promise },
  );
  await ui.flush();
  ui.close();
  pending.reject(new Error("unmounted probe"));
  await ui.flush();
});

test("launch pages pass actual root/instance settings, save callback and operation gates into the shared card", async () => {
  for (const target of ["SettingsPanel", "InstancePanel"]) {
    const saved = [];
    const ui = harness(
      target,
      {
        settings: base,
        instance: {
          id: "example",
          loader: "Vanilla",
          minecraft_version: "1.21",
          java_major: 21,
          isolated: false,
          mod_count: 0,
        },
        section: target === "SettingsPanel" ? "launch" : "settings",
        api: async () => info,
        native: true,
        disabled: false,
        onSave: async (value) => saved.push(value),
        onNotify() {},
      },
      {
        module:
          target === "InstancePanel"
            ? "InstancesPanel.tsx"
            : "SettingsPanel.tsx",
      },
    );
    const card = () =>
      elements(ui.tree, (node) => node.type?.name === "MemoryControl")[0];
    assert.equal(card().props.settings, base);
    assert.equal(
      card().props.instanceId,
      target === "InstancePanel" ? "example" : undefined,
    );
    assert.equal(card().props.disabled, false);
    await card().props.onSave({ ...base, memory_mode: "auto" });
    assert.equal(saved[0].memory_mode, "auto");
    ui.replace(
      target === "SettingsPanel"
        ? { downloadDisabled: true }
        : { mutationDisabled: true },
    );
    assert.equal(card().props.disabled, true);
    ui.close();
  }
});

function scopedApi(api, rootId) {
  const require = createRequire(import.meta.url);
  const ts = require("typescript");
  const source = fs.readFileSync(
    new URL("../src/main.tsx", import.meta.url),
    "utf8",
  );
  const tree = ts.createSourceFile(
    "main.tsx",
    source,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX,
  );
  function variable(name) {
    let found;
    function visit(node) {
      if (ts.isVariableDeclaration(node) && node.name.getText(tree) === name)
        found = node.getText(tree);
      if (!found) ts.forEachChild(node, visit);
    }
    visit(tree);
    assert.ok(found, `Missing production declaration ${name}`);
    return `const ${found};`;
  }
  const code = ts.transpileModule(
    `${variable("rootCommands")}
${variable("resourceWrites")}
${variable("rootApi")}
return rootApi;`,
    {
      compilerOptions: {
        target: ts.ScriptTarget.ES2022,
        module: ts.ModuleKind.CommonJS,
      },
    },
  ).outputText;
  const unexpected = () => {
    throw Error("Memory previews must not trigger writes or bootstrap");
  };
  return new Function(
    "api",
    "rootId",
    "React",
    "contextKey",
    "setResourceTarget",
    "load",
    "acceptJavaRegistration",
    code,
  )(
    api,
    rootId,
    { useMemo: (factory) => factory() },
    { current: rootId },
    unexpected,
    unexpected,
    unexpected,
  );
}
test("memory preview traverses production rootApi with the captured target", async () => {
  const calls = [];
  const api = scopedApi(async (command, args) => {
    calls.push({ command, args });
    return preview();
  }, "root-a");
  await api("memory_preview", { id: "example", rootId: "foreign" });
  await api("memory_preview", { id: null });
  assert.deepEqual(calls, [
    { command: "memory_preview", args: { id: "example", rootId: "root-a" } },
    { command: "memory_preview", args: { id: null, rootId: "root-a" } },
  ]);
});

test("refresh uses one new memory snapshot for allocation and the memory bar", async () => {
  const settings = { ...base, memory_mode: "auto" };
  let current = preview(settings);
  const { ui } = view({ settings }, { read: () => current });
  await ui.flush();
  const bar = () =>
    elements(ui.tree, (node) => node.props?.className === "ce-memory-bar")[0]
      .props.children.props.style.width;
  assert.equal(bar(), "50%");
  current = preview(settings, null, {
    memory_gib: 3,
    available_memory_bytes: 8 * 1073741824,
    constrained: true,
  });
  button(ui, "memory.refresh").props.onClick();
  await ui.flush();
  assert.match(allocation(ui), /^3\.0 GiB/);
  assert.equal(
    bar(),
    "75%",
    "refreshed allocation cannot keep the old available-memory bar",
  );
  current = preview(settings, null, {
    memory_gib: 0,
    error: "low available memory",
  });
  button(ui, "memory.refresh").props.onClick();
  await ui.flush();
  assert.match(allocation(ui), /^—/);
  assert.equal(
    bar(),
    "0%",
    "failed auto estimate must not display an unrelated old snapshot",
  );
  ui.close();
});
