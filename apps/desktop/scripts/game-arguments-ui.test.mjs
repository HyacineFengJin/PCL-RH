/** Actual field and parent handlers with synthetic delayed saves. No native
 * application, Java, game, account or user configuration is accessed. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { harness, elements, button, deferred } from "./maker-test-harness.mjs";

function view(extra = {}, respond = async () => {}) {
  const saved = [],
    notices = [];
  const ui = harness(
    "GameArgumentField",
    {
      field: "jvm_args",
      value: "",
      inherited: false,
      scope: "root-a:revision-one:example:jvm_args",
      draftKey: "root-a:example:jvm_args",
      disabled: false,
      onNotify: (message) => notices.push(message),
      onSave(value) {
        saved.push(value);
        return respond(value);
      },
      ...extra,
    },
    { module: "GameArgumentFields.tsx" },
  );
  return { ui, saved, notices };
}
function editor(ui) {
  const fields = elements(ui.tree, (node) =>
    ["input", "textarea"].includes(node.type),
  );
  assert.equal(fields.length, 1);
  return fields[0];
}
function select(ui, value) {
  const node = elements(ui.tree, (node) => node.type?.name === "CeSelect")[0];
  assert.ok(node);
  node.props.onChange({ target: { value } });
}
async function edit(ui, value) {
  editor(ui).props.onChange({ target: { value } });
  await ui.flush();
}
function save(ui) {
  return button(ui, "settings.saveArguments");
}

test("global arguments save explicitly and preserve quotes, paths and literal syntax", async () => {
  const { ui, saved } = view();
  const text = '-Dexample.path="C:\\Game Files\\config" -Dliteral=${example}';
  await edit(ui, text);
  assert.deepEqual(saved, []);
  assert.equal(editor(ui).type, "textarea");
  save(ui).props.onClick();
  await ui.flush();
  assert.deepEqual(saved, [text]);
  ui.replace({ value: text, scope: "root-a:revision-two:example:jvm_args" });
  await ui.flush();
  assert.equal(save(ui).props.disabled, true);
  ui.close();
});

test("instance inheritance, explicit empty override and restoration stay distinct", async () => {
  const { ui, saved } = view({ inherited: true, value: null });
  assert.equal(elements(ui.tree, (node) => node.type === "textarea").length, 0);
  select(ui, "custom");
  await ui.flush();
  assert.equal(editor(ui).props.value, "");
  assert.equal(save(ui).props.disabled, false);
  assert.deepEqual(saved, []);
  save(ui).props.onClick();
  await ui.flush();
  assert.deepEqual(saved, [""]);
  ui.replace({ value: "", scope: "root-a:revision-two:example:jvm_args" });
  await ui.flush();
  select(ui, "inherit");
  await ui.flush();
  assert.deepEqual(saved, ["", undefined]);
  ui.close();
});

test("unrelated revision preserves a draft while retiring its earlier save handler", async () => {
  const { ui, saved } = view();
  await edit(ui, "-Dunsaved=true");
  const stale = save(ui).props.onClick;
  ui.replace({ scope: "root-a:revision-two:example:jvm_args" });
  await ui.flush();
  assert.equal(editor(ui).props.value, "-Dunsaved=true");
  stale();
  await ui.flush();
  assert.deepEqual(saved, []);
  save(ui).props.onClick();
  await ui.flush();
  assert.deepEqual(saved, ["-Dunsaved=true"]);
  ui.close();
});

test("returning an unsaved independent field to inheritance cancels locally", async () => {
  const { ui, saved } = view({ inherited: true, value: null });
  select(ui, "custom");
  await ui.flush();
  await edit(ui, "-Ddiscarded=true");
  select(ui, "inherit");
  await ui.flush();
  assert.deepEqual(saved, []);
  assert.equal(elements(ui.tree, (node) => node.type === "textarea").length, 0);
  select(ui, "custom");
  await ui.flush();
  assert.equal(editor(ui).props.value, "");
  ui.close();
});

test("the preference writer's own busy transition does not swallow validation failure", async () => {
  const pending = deferred();
  const { ui, saved, notices } = view({}, () => pending.promise);
  await edit(ui, '-Dunfinished="');
  const click = save(ui).props.onClick;
  click();
  ui.replace({ busy: true });
  await ui.flush();
  ui.replace({ busy: false });
  pending.reject(new Error("Invalid JVM arguments: a quote is not closed"));
  await ui.flush();
  assert.equal(notices.length, 1);
  assert.match(notices[0], /quote is not closed/);
  assert.equal(editor(ui).props.value, '-Dunfinished="');
  click();
  await ui.flush();
  assert.equal(
    saved.length,
    1,
    "busy round trip must still retire earlier event handlers",
  );
  ui.close();
});

test("changing target or actual saved value replaces the draft", async () => {
  const { ui } = view();
  await edit(ui, "-Dold=true");
  ui.replace({
    draftKey: "root-b:example:jvm_args",
    scope: "root-b:revision-one:example:jvm_args",
  });
  await ui.flush();
  assert.equal(editor(ui).props.value, "");
  await edit(ui, "-Dother=true");
  ui.replace({
    value: "-Dexternal=true",
    scope: "root-b:revision-two:example:jvm_args",
  });
  await ui.flush();
  assert.equal(editor(ui).props.value, "-Dexternal=true");
  ui.close();
});

test("native validation failure keeps the editable draft without changing saved state", async () => {
  let reject = true;
  const { ui, saved, notices } = view({}, async () => {
    if (reject) throw new Error("Invalid JVM arguments: a quote is not closed");
  });
  await edit(ui, '-Dexample="unfinished');
  save(ui).props.onClick();
  await ui.flush();
  assert.equal(notices.length, 1);
  assert.equal(editor(ui).props.value, '-Dexample="unfinished');
  assert.equal(save(ui).props.disabled, false);
  reject = false;
  await edit(ui, "-Dexample=corrected");
  save(ui).props.onClick();
  await ui.flush();
  assert.deepEqual(saved, ['-Dexample="unfinished', "-Dexample=corrected"]);
  ui.close();
});

test("double clicks and inheritance cannot publish another save while the first is pending", async () => {
  const pending = deferred();
  const { ui, saved } = view(
    { inherited: true, value: "-Dold=true" },
    () => pending.promise,
  );
  await edit(ui, "-Dnew=true");
  const click = save(ui).props.onClick;
  click();
  click();
  select(ui, "inherit");
  await ui.flush();
  assert.deepEqual(saved, ["-Dnew=true"]);
  assert.equal(save(ui).props.disabled, true);
  pending.resolve();
  await ui.flush();
  ui.close();
});

for (const change of ["scope", "disabled"]) {
  test(`${change} round trips retire old edit, save and inheritance handlers`, async () => {
    const { ui, saved } = view({ inherited: true, value: "-Dold=true" });
    await edit(ui, "-Dnew=true");
    const click = save(ui).props.onClick;
    const input = editor(ui).props.onChange;
    const choose = elements(
      ui.tree,
      (node) => node.type?.name === "CeSelect",
    )[0].props.onChange;
    const original = change === "scope" ? ui.props.scope : false;
    ui.replace({ [change]: change === "scope" ? "root-b:revision-one" : true });
    await ui.flush();
    ui.replace({ [change]: original });
    await ui.flush();
    input({ target: { value: "-Dstale=true" } });
    click();
    choose({ target: { value: "inherit" } });
    await ui.flush();
    assert.deepEqual(saved, []);
    assert.equal(editor(ui).props.value, "-Dnew=true");
    save(ui).props.onClick();
    await ui.flush();
    assert.deepEqual(saved, ["-Dnew=true"]);
    ui.close();
  });
}

test("departing view keeps pending write locked and suppresses its late failure", async () => {
  const pending = deferred();
  const { ui, saved, notices } = view({}, () => pending.promise);
  await edit(ui, "-Dfirst=true");
  save(ui).props.onClick();
  ui.replace({
    scope: "root-b:revision-one",
    draftKey: "root-b:example:jvm_args",
  });
  await ui.flush();
  await edit(ui, "-Dsecond=true");
  save(ui).props.onClick();
  assert.deepEqual(saved, ["-Dfirst=true"]);
  pending.reject(new Error("Old view failure"));
  await ui.flush();
  assert.deepEqual(notices, []);
  ui.close();
});

test("custom version info uses Enter; parameter text uses Ctrl+Enter and preserves plain newline", async () => {
  for (const field of ["custom_info", "game_args"]) {
    const { ui, saved } = view({ field });
    await edit(ui, field === "custom_info" ? "Example" : "--server localhost");
    let prevented = false;
    editor(ui).props.onKeyDown({
      key: "Enter",
      preventDefault: () => {
        prevented = true;
      },
    });
    await ui.flush();
    assert.equal(prevented, field === "custom_info");
    if (field === "game_args") {
      assert.deepEqual(saved, []);
      editor(ui).props.onKeyDown({
        key: "Enter",
        ctrlKey: true,
        preventDefault() {},
      });
      await ui.flush();
    }
    assert.equal(saved.length, 1);
    ui.close();
  }
});

const settings = Object.freeze({
  root_id: "root-a",
  root: "/synthetic/game",
  revision: "revision-one",
  player: "Player",
  memory_gib: 8,
  selected: "example",
  overrides: {},
  argument_overrides: Object.freeze({
    example: Object.freeze({ jvm_args: "-Dexisting=true", custom_info: null }),
    other: Object.freeze({ game_args: "--other" }),
  }),
});

test("instance saves preserve sibling settings, and removing the last override leaves no empty reference", () => {
  const { ui } = view();
  const changed = harness(
    "withInstanceArgument",
    {},
    {
      module: "GameArgumentFields.tsx",
      render(module) {
        return module.withInstanceArgument(
          settings,
          "example",
          "game_args",
          "",
        );
      },
    },
  );
  assert.equal(
    changed.tree.argument_overrides.example.jvm_args,
    "-Dexisting=true",
  );
  assert.equal(changed.tree.argument_overrides.example.game_args, "");
  assert.deepEqual(
    changed.tree.argument_overrides.other,
    settings.argument_overrides.other,
  );
  assert.equal(settings.argument_overrides.example.game_args, undefined);
  const restored = harness(
    "withInstanceArgument",
    {},
    {
      module: "GameArgumentFields.tsx",
      render(module) {
        return module.withInstanceArgument(
          settings,
          "example",
          "jvm_args",
          undefined,
        );
      },
    },
  );
  assert.equal(restored.tree.argument_overrides.example, undefined);
  assert.deepEqual(
    restored.tree.argument_overrides.other,
    settings.argument_overrides.other,
  );
  changed.close();
  restored.close();
  ui.close();
});

test("global page submits each argument as a partial preference patch", async () => {
  const patches = [];
  const ui = harness("SettingsPanel", {
    section: "launch",
    settings,
    native: true,
    disabled: false,
    api: async () => ({
      total_memory_bytes: 16e9,
      available_memory_bytes: 8e9,
    }),
    launcherPreferences: {
      revision: "prefs-one",
      warning: null,
      preferences: { game_launch: { custom_info: "Global" } },
    },
    onLauncherPatch: async (patch) => {
      patches.push(patch);
    },
    onNotify() {},
    onSave: async () => {},
  });
  const fields = elements(
    ui.tree,
    (node) => node.type?.name === "GameArgumentField",
  );
  assert.equal(fields.length, 3);
  for (const field of fields) {
    assert.equal(field.props.disabled, false);
    await field.props.onSave("literal");
  }
  assert.deepEqual(patches, [
    { game_launch: { custom_info: "literal" } },
    { game_launch: { jvm_args: "literal" } },
    { game_launch: { game_args: "literal" } },
  ]);
  ui.close();
});

test("instance page binds each field to only the selected root and instance", async () => {
  const saves = [];
  const ui = harness(
    "InstancePanel",
    {
      instance: {
        id: "example",
        loader: "Fabric",
        minecraft_version: "1.21",
        java_major: 21,
        isolated: true,
        mod_count: 0,
      },
      section: "settings",
      settings,
      native: true,
      disabled: false,
      api: async () => ({
        total_memory_bytes: 16e9,
        available_memory_bytes: 8e9,
      }),
      onSave: async (value) => {
        saves.push(value);
      },
      onNotify() {},
    },
    { module: "InstancesPanel.tsx" },
  );
  const fields = elements(
    ui.tree,
    (node) => node.type?.name === "GameArgumentField",
  );
  assert.equal(fields.length, 3);
  for (const field of fields) {
    assert.equal(field.props.inherited, true);
    assert.equal(field.props.disabled, false);
    await field.props.onSave("");
    assert.equal(
      saves.at(-1).argument_overrides.example[field.props.field],
      "",
    );
    assert.deepEqual(
      saves.at(-1).argument_overrides.other,
      settings.argument_overrides.other,
    );
    assert.equal(saves.at(-1).revision, settings.revision);
  }
  ui.close();
});
