/** Diagnostic recommendations only choose a catalog entry; downloading still
 * requires an explicit click and retains the Java action's ownership checks. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { harness, button, field, deferred } from "./maker-test-harness.mjs";

const settings = {
  root_id: "fixture",
  root: "/synthetic/game",
  revision: "one",
};
const runtime = (major) => ({
  component: `runtime-${major}`,
  sha1: String(major % 10).repeat(40),
  version: `${major}.0`,
  major,
  platform: "linux",
  released: "2026-01-01",
});
const identity = (p) => `${p.component}:${p.sha1}`;
function view(packages, recommendedMajor) {
  const calls = [],
    tracked = [];
  const api = async (command, args) => {
    calls.push({ command, args });
    if (command === "java_download_catalog") return packages;
    if (command === "java_download_start") return { id: "task-one" };
    throw Error(command);
  };
  const ui = harness("JavaDownloads", {
    api,
    settings,
    native: true,
    disabled: false,
    recommendedMajor,
    onNotify() {},
    onTask: (...args) => tracked.push(args),
  });
  return { ui, calls, tracked };
}

test("recommended Java is preselected without starting a download", async () => {
  const java25 = runtime(25),
    java17 = runtime(17);
  const { ui, calls, tracked } = view([java25, java17], 17);
  await ui.flush();
  assert.equal(field(ui, "java.version").props.value, identity(java17));
  assert.deepEqual(
    calls.map((call) => call.command),
    ["java_download_catalog"],
  );
  button(ui, "java.startDownload").props.onClick();
  await ui.flush();
  assert.equal(calls[1].args.component, java17.component);
  assert.equal(calls[1].args.revision, settings.revision);
  assert.deepEqual(tracked, [["task-one", true]]);
  ui.close();
});

test("an absent recommended major does not silently download the first catalog entry", async () => {
  const java25 = runtime(25);
  const { ui, calls } = view([java25], 17);
  await ui.flush();
  const select = field(ui, "java.version");
  assert.equal(select.props.value, "");
  assert.equal(select.props.disabled, false);
  assert.equal(button(ui, "java.startDownload").props.disabled, true);
  button(ui, "java.startDownload").props.onClick();
  await ui.flush();
  assert.equal(calls.length, 1);
  select.props.onChange({ target: { value: identity(java25) } });
  await ui.flush();
  assert.equal(button(ui, "java.startDownload").props.disabled, false);
  ui.close();
});

test("a changed recommendation retires a saved download click", async () => {
  const java17 = runtime(17),
    java21 = runtime(21);
  const { ui, calls } = view([java17, java21], 17);
  await ui.flush();
  const oldClick = button(ui, "java.startDownload").props.onClick;
  field(ui, "java.version").props.onChange({
    target: { value: identity(java17) },
  });
  await ui.flush();
  ui.replace({ recommendedMajor: 21 });
  await ui.flush();
  assert.equal(field(ui, "java.version").props.value, identity(java21));
  oldClick();
  await ui.flush();
  assert.equal(
    calls.filter((call) => call.command === "java_download_start").length,
    0,
  );
  ui.close();
});

test("a late catalog cannot overwrite another root's recommendation", async () => {
  const pending = deferred();
  const { ui } = view(pending.promise, 17);
  await ui.flush();
  ui.replace({
    api: async () => [runtime(21)],
    settings: { ...settings, root_id: "other" },
    recommendedMajor: 21,
  });
  await ui.flush();
  pending.resolve([runtime(17)]);
  await ui.flush();
  assert.equal(field(ui, "java.version").props.value, identity(runtime(21)));
  ui.close();
});
