/** Account ownership regressions run real handlers/hooks without native login,
 * clipboard access, a keyring or remote texture requests. */
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  harness,
  elements,
  content,
  button,
  field,
  deferred,
} from "./maker-test-harness.mjs";

const profile = (skin = "one") => ({
  id: "account-1",
  name: "OnlinePlayer",
  uuid: "0".repeat(32),
  expires_at: 0,
  textures: { skin: { url: skin } },
});
const auth = (accounts = [{ profile: profile(), remembered: false }]) => ({
  client_id: "approved",
  selected: "account-1",
  accounts,
  stage: "success",
  message: "",
  challenge: null,
  warning: null,
  saveable: ["account-1"],
  saving: false,
});
const picture = (name) => ({
  avatar_png: name,
  skin_png: null,
  cape_png: null,
  cape_name: "",
  has_cape: false,
  warning: null,
});
function presentation(api, state = auth()) {
  return harness(
    "useAccountPresentation",
    { api, native: true, auth: state },
    {
      module: "useAccountPresentation.ts",
      render: (m, p) => m.useAccountPresentation(p.api, p.native, p.auth),
    },
  );
}
function dialog(mode, extra = {}) {
  const actions = [],
    saves = [];
  const ui = harness("AccountDialog", {
    mode,
    player: "OfflinePlayer",
    auth: auth(),
    native: true,
    busy: false,
    working: false,
    error: "",
    images: {},
    assist: null,
    remember: true,
    onRemember() {},
    onBack() {},
    onClose() {},
    onAppearance() {},
    onAction: async (command, args) => {
      actions.push({ command, args });
    },
    onOfflineSave: async (name) => {
      saves.push(name);
    },
    ...extra,
  });
  return { ui, actions, saves };
}

test("offline child contains only its name and saves a validated draft", async () => {
  const { ui, actions, saves } = dialog("offline");
  assert.equal(elements(ui.tree, (n) => n.type === "input").length, 1);
  assert.ok(!content(ui.tree).includes("OnlinePlayer"));
  assert.ok(!content(ui.tree).includes(ui.i18n.t("accounts.signIn")));
  field(ui, "accounts.offlineName").props.onChange({
    target: { value: "bad name" },
  });
  await ui.flush();
  const submit = () =>
    elements(ui.tree, (n) => n.type === "form")[0].props.onSubmit({
      preventDefault() {},
    });
  submit();
  await ui.flush();
  assert.equal(saves.length, 0);
  assert.equal(elements(ui.tree, (n) => n.props?.role === "alert").length, 1);
  field(ui, "accounts.offlineName").props.onChange({
    target: { value: "Valid_Name" },
  });
  await ui.flush();
  submit();
  await ui.flush();
  assert.deepEqual(saves, ["Valid_Name"]);
  assert.deepEqual(actions, []);
  ui.close();
});

test("premium child excludes offline/developer fields and saves the matching live account", async () => {
  const { ui, actions } = dialog("premium");
  assert.ok(!content(ui.tree).includes("OfflinePlayer"));
  assert.equal(
    elements(ui.tree, (n) => n.type === "input" && n.props.type !== "checkbox")
      .length,
    0,
  );
  button(ui, "accounts.save").props.onClick();
  await ui.flush();
  assert.deepEqual(actions, [
    { command: "auth_save_login", args: { id: "account-1" } },
  ]);
  ui.replace({ auth: { ...auth(), saveable: [] } });
  assert.equal(button(ui, "accounts.save").props.disabled, true);
  ui.replace({ auth: { ...auth(), saving: true, stage: "saving" } });
  assert.equal(button(ui, "accounts.save").props.disabled, true);
  assert.equal(button(ui, "accounts.signIn").props.disabled, true);
  ui.close();
});

test("code panel has native copy/open/cancel actions without duplicating login controls", async () => {
  const code = "ABCD1234",
    { ui, actions } = dialog("premium", {
      auth: {
        ...auth([]),
        selected: null,
        stage: "waiting",
        challenge: { user_code: code },
      },
      assist: { attempted: true, copied: true, opened: true },
    });
  assert.ok(content(ui.tree).includes(code));
  assert.ok(content(ui.tree).includes(ui.i18n.t("accounts.codeCopied")));
  assert.ok(!content(ui.tree).includes(ui.i18n.t("accounts.signIn")));
  for (const key of [
    "accounts.copyCode",
    "accounts.browserShort",
    "common.cancel",
  ])
    button(ui, key).props.onClick();
  await ui.flush();
  assert.deepEqual(actions, [
    { command: "auth_copy_code", args: { code } },
    { command: "auth_open_browser", args: undefined },
    { command: "auth_cancel", args: undefined },
  ]);
  ui.close();
});

test("auth polling reuses appearance and does not follow token expiry/selection", async () => {
  const calls = [],
    ui = presentation(async (command, args) => {
      calls.push({ command, args });
      return picture("avatar-one");
    });
  await ui.flush();
  assert.equal(ui.tree.images["account-1"].avatar_png, "avatar-one");
  ui.replace({
    auth: {
      ...auth(),
      selected: null,
      accounts: [
        { profile: { ...profile(), expires_at: 100 }, remembered: true },
      ],
    },
  });
  await ui.flush();
  assert.equal(calls.length, 1);
  await ui.tree.refresh("account-1");
  await ui.flush();
  assert.equal(calls.length, 2);
  assert.equal(calls[1].args.refresh, true);
  ui.close();
});

test("replacement/deletion/unmount retire stale appearance responses", async () => {
  const requests = [],
    ui = presentation(() => {
      const d = deferred();
      requests.push(d);
      return d.promise;
    });
  await ui.flush();
  ui.replace({ auth: auth([{ profile: profile("two"), remembered: false }]) });
  await ui.flush();
  assert.equal(requests.length, 2);
  requests[1].resolve(picture("new"));
  await ui.flush();
  requests[0].resolve(picture("stale"));
  await ui.flush();
  assert.equal(ui.tree.images["account-1"].avatar_png, "new");
  ui.replace({
    auth: auth([{ profile: profile("three"), remembered: false }]),
  });
  await ui.flush();
  assert.equal(ui.tree.images["account-1"], undefined);
  ui.replace({ auth: auth([]) });
  await ui.flush();
  requests[2].resolve(picture("removed"));
  await ui.flush();
  assert.deepEqual(ui.tree.images, {});
  ui.replace({ auth: auth() });
  await ui.flush();
  ui.close();
  requests[3].resolve(picture("closed"));
  await ui.flush();
  assert.deepEqual(ui.tree.images, {});
});

test("overlapping retries share the active request and failed images do not erase login", async () => {
  const d = deferred(),
    calls = [],
    ui = presentation((command) => {
      calls.push(command);
      return d.promise;
    });
  await ui.flush();
  await ui.tree.refresh("account-1");
  await ui.tree.refresh("account-1");
  assert.equal(calls.length, 1);
  d.reject(new Error("offline"));
  await ui.flush();
  assert.deepEqual(ui.tree.images, {});
  assert.equal(ui.props.auth.selected, "account-1");
  ui.close();
});

test("automatic assistance runs once per challenge and cancelled replies stay retired", async () => {
  const calls = [],
    requests = [],
    ui = presentation((command, args) => {
      calls.push({ command, args });
      const d = deferred();
      requests.push(d);
      return d.promise;
    }, auth([]));
  const challenge = { user_code: "ABCD1234" };
  ui.replace({ auth: { ...auth([]), stage: "waiting", challenge } });
  await ui.flush();
  ui.replace({
    auth: {
      ...auth([]),
      stage: "waiting",
      challenge: { ...challenge },
      message: "polled",
    },
  });
  await ui.flush();
  assert.equal(calls.length, 1);
  ui.replace({ auth: auth([]) });
  await ui.flush();
  requests[0].resolve({ attempted: true, copied: true, opened: true });
  await ui.flush();
  assert.equal(ui.tree.assist, null);
  ui.replace({
    auth: {
      ...auth([]),
      stage: "waiting",
      challenge: { user_code: "EFGH5678" },
    },
  });
  await ui.flush();
  requests[1].resolve({ attempted: true, copied: false, opened: true });
  await ui.flush();
  assert.deepEqual(ui.tree.assist, {
    attempted: true,
    copied: false,
    opened: true,
  });
  assert.deepEqual(
    calls.map((c) => c.args.code),
    ["ABCD1234", "EFGH5678"],
  );
  ui.close();
});
