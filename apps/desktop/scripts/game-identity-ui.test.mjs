/** Real game identity field hooks and handlers, with synthetic settings and
 * delayed saves. No native API, filesystem, application or game is invoked. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { button, deferred, elements, harness } from "./maker-test-harness.mjs";

const settings = Object.freeze({
  root_id: "root-a",
  root: "/synthetic/game",
  revision: "revision-one",
  isolation: "automatic",
  isolation_overrides: Object.freeze({ example: "isolated", other: "shared" }),
  window_title_overrides: Object.freeze({ example: "Existing title" }),
  selected_version: "example",
  memory_gib: 8,
  roots: Object.freeze([{ id: "root-a", path: "/synthetic/game" }]),
});

function view(name, props = {}, respond = async () => {}) {
  const saved = [],
    notices = [];
  const ui = harness(
    name,
    {
      scope: "root-a:revision-one:example",
      disabled: false,
      onNotify: (message) => notices.push(message),
      onSave: (value) => {
        saved.push(structuredClone(value));
        return respond(value);
      },
      ...(name === "IsolationField"
        ? { settings, instanceId: "example" }
        : { value: undefined, inherited: true }),
      ...props,
    },
    { module: "GameIdentityFields.tsx" },
  );
  return { ui, saved, notices };
}

function select(ui) {
  const result = elements(ui.tree, (node) => node.type?.name === "CeSelect");
  assert.equal(result.length, 1);
  return result[0];
}

function input(ui) {
  const result = elements(ui.tree, (node) => node.type === "input");
  assert.equal(result.length, 1);
  return result[0];
}

function choose(ui, value) {
  select(ui).props.onChange({ target: { value } });
}

async function edit(ui, value) {
  input(ui).props.onChange({ target: { value } });
  await ui.flush();
}

function enter(handler) {
  let prevented = false;
  handler({ key: "Enter", preventDefault: () => (prevented = true) });
  assert.equal(prevented, true);
}

test("instance isolation follows global by removing only its override", async () => {
  const { ui, saved, notices } = view("IsolationField");
  assert.equal(select(ui).props.value, "isolated");
  choose(ui, "inherit");
  await ui.flush();
  assert.deepEqual(saved, [
    { ...settings, isolation_overrides: { other: "shared" } },
  ]);
  assert.deepEqual(settings.isolation_overrides, {
    example: "isolated",
    other: "shared",
  });
  assert.deepEqual(notices, []);
  ui.close();
});

test("explicit isolation choices save only policy and preserve all content metadata", async () => {
  for (const policy of ["automatic", "shared", "isolated"]) {
    const { ui, saved } = view("IsolationField");
    choose(ui, policy);
    await ui.flush();
    assert.deepEqual(saved, [
      {
        ...settings,
        isolation_overrides: {
          ...settings.isolation_overrides,
          example: policy,
        },
      },
    ]);
    // Passing the exact immutable content/root metadata through the one save
    // callback proves this control submits policy, not a move/copy/delete plan.
    assert.equal(saved[0].root, settings.root);
    assert.deepEqual(
      saved[0].window_title_overrides,
      settings.window_title_overrides,
    );
    ui.close();
  }
});

test("global isolation has no inherit choice and invalid values never save", async () => {
  const { ui, saved } = view("IsolationField", { instanceId: undefined });
  assert.equal(select(ui).props.value, "automatic");
  assert.ok(
    !elements(select(ui), (node) => node.type === "option").some(
      (node) => node.props.value === "inherit",
    ),
  );
  for (const invalid of ["inherit", "move-files", "", "ISOLATED"])
    choose(ui, invalid);
  await ui.flush();
  assert.deepEqual(saved, []);
  choose(ui, "shared");
  await ui.flush();
  assert.deepEqual(saved, [{ ...settings, isolation: "shared" }]);
  ui.close();
});

test("title inheritance and an explicit empty title remain distinct saves", async () => {
  const { ui, saved } = view("GameTitleField");
  assert.equal(select(ui).props.value, "inherit");
  choose(ui, "default");
  await ui.flush();
  assert.deepEqual(saved, [""]);
  ui.replace({ value: "", scope: "root-a:revision-two:example" });
  await ui.flush();
  assert.equal(select(ui).props.value, "default");
  choose(ui, "inherit");
  await ui.flush();
  assert.deepEqual(saved, ["", undefined]);
  ui.replace({ value: undefined, scope: "root-a:revision-three:example" });
  await ui.flush();
  assert.equal(select(ui).props.value, "inherit");
  ui.close();
});

test("global title rejects inherit and opening custom editor does not save", async () => {
  const { ui, saved } = view("GameTitleField", { inherited: false, value: "" });
  assert.equal(select(ui).props.value, "default");
  choose(ui, "inherit");
  choose(ui, "unknown");
  choose(ui, "custom");
  await ui.flush();
  assert.equal(select(ui).props.value, "custom");
  assert.equal(input(ui).props.value, "");
  assert.equal(button(ui, "settings.saveTitle").props.disabled, true);
  assert.deepEqual(saved, []);
  ui.close();
});

test("Unicode title saves literal whitespace, quotes and shell syntax through one callback", async () => {
  const title = '  测试游戏 🦀 "literal" \\ $(no shell)  ';
  const { ui, saved, notices } = view("GameTitleField");
  choose(ui, "custom");
  await ui.flush();
  await edit(ui, title);
  assert.equal(button(ui, "settings.saveTitle").props.disabled, false);
  enter(input(ui).props.onKeyDown);
  await ui.flush();
  assert.deepEqual(saved, [title]);
  assert.deepEqual(notices, []);
  ui.close();
});

test("title validation counts Unicode code points and rejects controls without saving", async () => {
  const { ui, saved, notices } = view("GameTitleField", { value: "original" });
  for (const invalid of [
    "  ",
    "a\nb",
    "\u0000title",
    "\u007f",
    "\u0085",
    "🦀".repeat(257),
  ]) {
    await edit(ui, invalid);
    enter(input(ui).props.onKeyDown);
    await ui.flush();
    assert.deepEqual(saved, []);
    assert.equal(notices.at(-1), ui.i18n.t("settings.titleInvalid"));
  }
  await edit(ui, "🦀".repeat(256));
  button(ui, "settings.saveTitle").props.onClick();
  await ui.flush();
  assert.deepEqual(saved, ["🦀".repeat(256)]);
  assert.equal(notices.length, 6);
  ui.close();
});

test("title click, Enter and mode changes cannot submit twice while a save is pending", async () => {
  const pending = deferred();
  const { ui, saved } = view(
    "GameTitleField",
    { value: "original" },
    () => pending.promise,
  );
  await edit(ui, "new title");
  const click = button(ui, "settings.saveTitle").props.onClick,
    key = input(ui).props.onKeyDown;
  click();
  click();
  enter(key);
  choose(ui, "inherit");
  await ui.flush();
  assert.deepEqual(saved, ["new title"]);
  assert.equal(select(ui).props.disabled, true);
  assert.equal(input(ui).props.disabled, true);
  assert.equal(button(ui, "settings.saveTitle").props.disabled, true);
  pending.resolve();
  await ui.flush();
  assert.equal(select(ui).props.disabled, false);
  ui.close();
});

test("isolation synchronous duplicate handlers submit once and recover after failure", async () => {
  const pending = deferred();
  let first = true;
  const { ui, saved, notices } = view("IsolationField", {}, () => {
    if (first) {
      first = false;
      return pending.promise;
    }
    return Promise.resolve();
  });
  const old = select(ui).props.onChange;
  old({ target: { value: "shared" } });
  old({ target: { value: "automatic" } });
  await ui.flush();
  assert.equal(saved.length, 1);
  assert.equal(select(ui).props.disabled, true);
  pending.reject(Error("Synthetic settings conflict"));
  await ui.flush();
  assert.deepEqual(notices, ["Error: Synthetic settings conflict"]);
  assert.equal(select(ui).props.disabled, false);
  choose(ui, "automatic");
  await ui.flush();
  assert.equal(saved.length, 2);
  assert.equal(saved[1].isolation_overrides.example, "automatic");
  ui.close();
});

test("failed title save keeps the draft for a deliberate retry", async () => {
  let failures = 1;
  const { ui, saved, notices } = view(
    "GameTitleField",
    { value: "original" },
    async () => {
      if (failures--) throw Error("Synthetic title conflict");
    },
  );
  await edit(ui, "retry this title");
  button(ui, "settings.saveTitle").props.onClick();
  await ui.flush();
  assert.equal(input(ui).props.value, "retry this title");
  assert.equal(button(ui, "settings.saveTitle").props.disabled, false);
  assert.deepEqual(notices, ["Error: Synthetic title conflict"]);
  button(ui, "settings.saveTitle").props.onClick();
  await ui.flush();
  assert.deepEqual(saved, ["retry this title", "retry this title"]);
  ui.close();
});

for (const name of ["IsolationField", "GameTitleField"]) {
  test(`${name} retires handlers after disabled → enabled`, async () => {
    const { ui, saved } = view(name, { value: "original" });
    const oldSelect = select(ui).props.onChange;
    const oldInput =
      name === "GameTitleField" ? input(ui).props.onChange : null;
    const oldClick =
      name === "GameTitleField"
        ? button(ui, "settings.saveTitle").props.onClick
        : null;
    ui.replace({ disabled: true });
    oldSelect({
      target: { value: name === "IsolationField" ? "shared" : "default" },
    });
    await ui.flush();
    assert.equal(select(ui).props.disabled, true);
    ui.replace({ disabled: false });
    oldSelect({
      target: { value: name === "IsolationField" ? "shared" : "default" },
    });
    oldInput?.({ target: { value: "stale draft" } });
    oldClick?.();
    await ui.flush();
    assert.deepEqual(saved, []);
    if (oldInput) assert.equal(input(ui).props.value, "original");
    choose(ui, name === "IsolationField" ? "shared" : "default");
    await ui.flush();
    assert.equal(saved.length, 1);
    ui.close();
  });

  test(`${name} root A → B → A does not revive the first root's handlers`, async () => {
    const { ui, saved } = view(name, { value: "original" });
    const oldSelect = select(ui).props.onChange;
    const oldInput =
      name === "GameTitleField" ? input(ui).props.onChange : null;
    ui.replace({ scope: "root-b:revision-one:example", value: "other root" });
    await ui.flush();
    ui.replace({ scope: "root-a:revision-one:example", value: "original" });
    await ui.flush();
    oldSelect({
      target: { value: name === "IsolationField" ? "shared" : "default" },
    });
    oldInput?.({ target: { value: "stale draft" } });
    await ui.flush();
    assert.deepEqual(saved, []);
    if (oldInput) assert.equal(input(ui).props.value, "original");
    ui.close();
  });

  test(`${name} close retires pending errors and retained handlers`, async () => {
    const pending = deferred();
    const { ui, saved, notices } = view(
      name,
      { value: "original" },
      () => pending.promise,
    );
    const old = select(ui).props.onChange;
    old({
      target: { value: name === "IsolationField" ? "shared" : "default" },
    });
    await ui.flush();
    assert.equal(saved.length, 1);
    ui.close();
    pending.reject(Error("Closed field error"));
    await ui.flush();
    old({
      target: { value: name === "IsolationField" ? "automatic" : "inherit" },
    });
    await ui.flush();
    assert.equal(saved.length, 1);
    assert.deepEqual(notices, []);
  });

  test(`${name} late failure from another scope cannot notify the current root`, async () => {
    const pending = deferred();
    const { ui, saved, notices } = view(
      name,
      { value: "original" },
      () => pending.promise,
    );
    choose(ui, name === "IsolationField" ? "shared" : "default");
    await ui.flush();
    ui.replace({ scope: "root-b:revision-one:example", value: "other root" });
    await ui.flush();
    pending.reject(Error("Retired root save failed"));
    await ui.flush();
    assert.deepEqual(notices, []);
    assert.equal(select(ui).props.disabled, false);
    assert.equal(saved.length, 1);
    if (name === "GameTitleField")
      assert.equal(input(ui).props.value, "other root");
    ui.close();
  });

  test(`${name} disabling retires an admitted save's failure without admitting concurrent writes`, async () => {
    const pending = deferred();
    let first = true;
    const { ui, saved, notices } = view(name, { value: "original" }, () => {
      if (first) {
        first = false;
        return pending.promise;
      }
      return Promise.resolve();
    });
    choose(ui, name === "IsolationField" ? "shared" : "default");
    await ui.flush();
    ui.replace({ disabled: true });
    ui.replace({ disabled: false });
    choose(ui, name === "IsolationField" ? "automatic" : "inherit");
    await ui.flush();
    assert.equal(saved.length, 1);
    assert.equal(select(ui).props.disabled, true);
    pending.reject(Error("Retired disabled field failed"));
    await ui.flush();
    assert.deepEqual(notices, []);
    assert.equal(select(ui).props.disabled, false);
    choose(ui, name === "IsolationField" ? "automatic" : "inherit");
    await ui.flush();
    assert.equal(saved.length, 2);
    ui.close();
  });
}
