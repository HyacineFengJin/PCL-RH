/** Exercise the actual mounted DownloadPanel hooks and task handlers with
 * in-memory native replies. No server, browser, download or game is started. */
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  communityHarness,
  content,
  deferred,
  elements,
} from "./community-test-harness.mjs";

const version = (id, kind = "release") => ({
  id,
  kind,
  release_time: "2026-01-01T00:00:00Z",
});
const rows = (ui) =>
  elements(
    ui.tree,
    (node) =>
      node.type === "button" &&
      node.props.className?.includes("ce-version-row"),
  );
const selection = (ui) =>
  elements(ui.tree, (node) => node.type?.name === "InstallSelection")[0];
function reload(ui) {
  return elements(
    ui.tree,
    (node) =>
      node.type === "button" && content(node) === ui.i18n.t("ui.reload"),
  )[0];
}
function fixture(overrides = {}) {
  const calls = [],
    requests = [],
    tracked = [],
    task = deferred();
  const api = async (command, args) => {
    calls.push({ command, args });
    if (command === "download_catalog") {
      const reply = deferred();
      requests.push(reply);
      return reply.promise;
    }
    if (command === "download_start") return task.promise;
    throw Error(`Unexpected synthetic command ${command}`);
  };
  const ui = communityHarness("DownloadPanel", {
    api,
    native: true,
    rootId: "root-a",
    rootAvailable: true,
    installed: [],
    gameBusy: false,
    visible: false,
    section: "minecraft",
    onResourceDetails() {},
    onTaskStart: (id) => tracked.push(id),
    ...overrides,
  });
  return { ui, api, calls, requests, tracked, task };
}

test("hidden startup does not request or construct a version list; visits share one request and result", async (t) => {
  const f = fixture();
  t.after(() => f.ui.close());
  await f.ui.flush();
  assert.equal(f.calls.length, 0);
  assert.equal(rows(f.ui).length, 0);
  f.ui.replace({ gameBusy: true });
  await f.ui.flush();
  assert.equal(f.calls.length, 0);
  f.ui.replace({ gameBusy: false, visible: true });
  await f.ui.flush();
  assert.equal(f.requests.length, 1);
  f.ui.replace({ visible: false });
  await f.ui.flush();
  f.ui.replace({ visible: true });
  await f.ui.flush();
  assert.equal(f.requests.length, 1, "Showing an in-flight catalog reuses it");
  f.requests[0].resolve([version("1.21.4")]);
  await f.ui.flush();
  assert.ok(rows(f.ui).some((row) => content(row).includes("1.21.4")));
  f.ui.replace({ visible: false });
  await f.ui.flush();
  assert.equal(rows(f.ui).length, 0);
  f.ui.replace({ visible: true });
  await f.ui.flush();
  assert.equal(f.requests.length, 1);
  assert.ok(rows(f.ui).some((row) => content(row).includes("1.21.4")));
});

test("a catalog reply admitted before hiding remains available without hidden row construction", async (t) => {
  const f = fixture({ visible: true });
  t.after(() => f.ui.close());
  f.ui.replace({ visible: false });
  await f.ui.flush();
  let filterCalls = 0;
  const entries = Array.from({ length: 1000 }, (_, index) =>
    version(`fixture-${index}`),
  );
  entries.filter = (...args) => {
    filterCalls += 1;
    return Array.prototype.filter.apply(entries, args);
  };
  f.requests[0].resolve(entries);
  await f.ui.flush();
  assert.equal(rows(f.ui).length, 0);
  const initialGrouping = filterCalls;
  for (let index = 0; index < 5; index++) {
    f.ui.replace({ gameBusy: index % 2 === 0 });
    await f.ui.flush();
  }
  assert.equal(
    filterCalls,
    initialGrouping,
    "Hidden status renders do not regroup 1000 entries",
  );
  assert.equal(rows(f.ui).length, 0);
  f.ui.replace({ visible: true, gameBusy: false });
  await f.ui.flush();
  assert.equal(f.requests.length, 1);
  assert.ok(rows(f.ui).length >= 1000);
});

test("community, favorites and loader-specific pages do not request the vanilla catalog", async (t) => {
  const f = fixture({ visible: true, section: "mods" });
  t.after(() => f.ui.close());
  await f.ui.flush();
  for (const section of [
    "Forge",
    "Fabric",
    "NeoForge",
    "favorites",
    "OptiFine",
  ]) {
    f.ui.replace({ section });
    await f.ui.flush();
  }
  assert.equal(f.calls.length, 0);
  f.ui.replace({ section: "installer-minecraft" });
  await f.ui.flush();
  assert.equal(
    f.requests.length,
    1,
    "Standalone Minecraft installer needs the vanilla catalog",
  );
  f.requests[0].resolve([version("1.21.4")]);
  await f.ui.flush();
  f.ui.replace({ section: "minecraft" });
  await f.ui.flush();
  assert.equal(f.requests.length, 1);
  assert.ok(rows(f.ui).some((row) => content(row).includes("1.21.4")));
});

test("API changes retire old catalog data and replies even when the root ID stays the same", async (t) => {
  const f = fixture({ visible: true }),
    next = deferred(),
    calls = [];
  t.after(() => f.ui.close());
  f.ui.replace({
    api: async (command, args) => {
      calls.push({ command, args });
      return next.promise;
    },
  });
  await f.ui.flush();
  assert.equal(calls.length, 1);
  next.resolve([version("new-api")]);
  await f.ui.flush();
  f.requests[0].resolve([version("retired-api")]);
  await f.ui.flush();
  assert.ok(rows(f.ui).some((row) => content(row).includes("new-api")));
  assert.ok(rows(f.ui).every((row) => !content(row).includes("retired-api")));
});

test("root changes retire old replies before another request is admitted", async (t) => {
  const f = fixture({ visible: true });
  t.after(() => f.ui.close());
  f.ui.replace({ rootId: "root-b", visible: false });
  await f.ui.flush();
  f.requests[0].resolve([version("root-a-only")]);
  await f.ui.flush();
  assert.equal(rows(f.ui).length, 0);
  assert.equal(
    f.requests.length,
    1,
    "Hidden root changes do not start network work",
  );
  f.ui.replace({ visible: true });
  await f.ui.flush();
  assert.equal(f.requests.length, 2);
  assert.ok(rows(f.ui).every((row) => !content(row).includes("root-a-only")));
  f.requests[1].resolve([version("root-b-only")]);
  await f.ui.flush();
  assert.ok(rows(f.ui).some((row) => content(row).includes("root-b-only")));
});

test("failed requests stay retryable without navigation loops; manual reload requests fresh data", async (t) => {
  const f = fixture({ visible: true });
  t.after(() => f.ui.close());
  f.requests[0].reject(Error("synthetic offline catalog"));
  await f.ui.flush();
  assert.ok(content(f.ui.tree).includes("synthetic offline catalog"));
  f.ui.replace({ visible: false });
  await f.ui.flush();
  f.ui.replace({ visible: true });
  await f.ui.flush();
  assert.equal(f.requests.length, 1);
  reload(f.ui).props.onClick();
  await f.ui.flush();
  assert.equal(f.requests.length, 2);
  assert.deepEqual(f.calls[1], {
    command: "download_catalog",
    args: { refresh: true },
  });
  f.requests[1].resolve([version("after-retry")]);
  await f.ui.flush();
  assert.ok(!content(f.ui.tree).includes("synthetic offline catalog"));
  assert.ok(rows(f.ui).some((row) => content(row).includes("after-retry")));
});

test("an admitted installation completes while hidden without cancellation or reopening its old view", async (t) => {
  const f = fixture({ visible: true });
  t.after(() => f.ui.close());
  f.requests[0].resolve([version("1.21.4")]);
  await f.ui.flush();
  rows(f.ui)[0].props.onClick();
  await f.ui.flush();
  const start = selection(f.ui).props.onStart;
  start({ name: "synthetic-installed", components: [] });
  await f.ui.flush();
  assert.equal(selection(f.ui).props.disabled, true);
  assert.deepEqual(f.calls[1], {
    command: "download_start",
    args: {
      id: "1.21.4",
      rootId: "root-a",
      name: "synthetic-installed",
      components: [],
    },
  });
  f.ui.replace({ visible: false });
  await f.ui.flush();
  assert.ok(
    selection(f.ui),
    "The install form remains owned while its submission completes",
  );
  f.task.resolve("owned-task");
  await f.ui.flush();
  assert.equal(selection(f.ui).props.disabled, false);
  assert.deepEqual(
    f.tracked,
    [],
    "Hidden completion cannot navigate to an old task view",
  );
  assert.deepEqual(
    f.calls.map((call) => call.command),
    ["download_catalog", "download_start"],
  );
  f.ui.replace({ visible: true, rootId: "root-b" });
  await f.ui.flush();
  start({ name: "must-not-use-new-root", components: [] });
  await f.ui.flush();
  assert.equal(
    f.calls.filter((call) => call.command === "download_start").length,
    1,
  );
});

test("current installation success still reveals its task and an unmounted catalog reply is inert", async (t) => {
  const f = fixture({ visible: true });
  t.after(() => f.ui.close());
  f.requests[0].resolve([version("1.21.4")]);
  await f.ui.flush();
  rows(f.ui)[0].props.onClick();
  await f.ui.flush();
  selection(f.ui).props.onStart({
    name: "synthetic-installed",
    components: [],
  });
  await f.ui.flush();
  f.task.resolve("owned-task");
  await f.ui.flush();
  assert.deepEqual(f.tracked, ["owned-task"]);
  assert.equal(selection(f.ui), undefined);
  const hidden = fixture({ visible: true });
  hidden.ui.close();
  hidden.requests[0].resolve([version("after-unmount")]);
  await hidden.ui.flush();
  assert.equal(rows(hidden.ui).length, 0);
});
