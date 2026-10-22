/** Actual announcement component with real Kernel-issued activation lifetimes.
 * Service promises are deterministic; no native IPC, clipboard or network runs. */
import assert from "node:assert/strict";
import test from "node:test";
import { harness, elements, content, deferred } from "./maker-test-harness.mjs";
import {
  own,
  plugin,
  createLauncherSurfacePort,
} from "./launcher-surface-fixture.mjs";
async function fixture(t) {
  const kernel = own(t, [plugin()]);
  await kernel.configure([], ["launcher"]);
  const port = createLauncherSurfacePort();
  t.after(port.attach(kernel));
  const requests = [],
    calls = [],
    notifications = [];
  const connection = {
    read(refresh) {
      calls.push({ command: "read", refresh });
      const request = deferred();
      requests.push(request);
      return request.promise;
    },
    openRelease(url) {
      calls.push({ command: "open", url });
      return Promise.resolve();
    },
  };
  let host = {
    announcements: {
      connection,
      available: true,
      scope: "all",
      policy: "all:fixture",
      localization: { language: "en-US", region: "US" },
      notify: (message) => notifications.push(message),
    },
  };
  const ui = harness(
    "AnnouncementSurface",
    {
      host,
      lifetime: port.list("home.announcements")[0].lifetime,
    },
    { module: "plugins/home/AnnouncementSurface.tsx" },
  );
  t.after(() => ui.close());
  return {
    ui,
    kernel,
    port,
    requests,
    calls,
    notifications,
    update(patch) {
      host = { announcements: { ...host.announcements, ...patch } };
      ui.replace({ host });
    },
    async disable() {
      await kernel.configure(["feed"], ["launcher"]);
      ui.replace({ host });
    },
    async enable() {
      await kernel.configure([], ["launcher"]);
      ui.replace({
        host,
        lifetime: port.list("home.announcements")[0].lifetime,
      });
    },
    get host() {
      return host;
    },
  };
}
const response = (title) => ({
  state: "ready",
  scope: "all",
  items: [
    { id: title, title, text: title, url: "https://example.invalid/release" },
  ],
  cached: false,
  truncated: false,
});
const refresh = (ui) =>
  elements(
    ui.tree,
    (n) =>
      n.type === "button" && /Refresh announcements|刷新公告/.test(content(n)),
  )[0];
const link = (ui) =>
  elements(ui.tree, (n) => n.type === "button" && n !== refresh(ui))[0];

test("disable removes content and rejects replies from the previous real activation", async (t) => {
  const f = await fixture(t);
  await f.ui.flush();
  await f.disable();
  f.requests[0].resolve(response("retired reply"));
  await f.ui.flush();
  assert.equal(f.ui.tree, null);
  await f.enable();
  await f.ui.flush();
  assert.equal(f.requests.length, 2);
  f.requests[1].resolve(response("current reply"));
  await f.ui.flush();
  assert.ok(content(f.ui.tree).includes("current reply"));
  assert.ok(!content(f.ui.tree).includes("retired reply"));
  await f.disable();
  assert.equal(f.ui.tree, null);
});

test("saved refresh and release callbacks cannot revive after reenabling", async (t) => {
  const f = await fixture(t);
  f.requests[0].resolve(response("initial"));
  await f.ui.flush();
  const savedRefresh = refresh(f.ui).props.onClick,
    savedLink = link(f.ui).props.onClick;
  await f.disable();
  await f.enable();
  await f.ui.flush();
  f.requests[1].resolve(response("new activation"));
  await f.ui.flush();
  const count = f.calls.length;
  savedRefresh();
  savedLink();
  await f.ui.flush();
  assert.equal(f.calls.length, count);
});

test("policy changes hide stale data before effects or a replacement read settle", async (t) => {
  const f = await fixture(t);
  f.requests[0].resolve(response("old policy"));
  await f.ui.flush();
  const saved = refresh(f.ui).props.onClick;
  f.update({ policy: "important:new-network", scope: "important" });
  assert.ok(!content(f.ui.tree).includes("old policy"));
  await f.ui.flush();
  assert.equal(f.requests.length, 2);
  saved();
  await f.ui.flush();
  assert.equal(f.requests.length, 2);
  f.requests[1].resolve(response("new policy"));
  await f.ui.flush();
  assert.ok(content(f.ui.tree).includes("new policy"));
});

test("an old reply cannot release the pending gate for a newer connection", async (t) => {
  const f = await fixture(t);
  const replacement = {
    ...f.host.announcements.connection,
    read: (refresh) => {
      f.calls.push({ command: "replacement", refresh });
      const d = deferred();
      f.requests.push(d);
      return d.promise;
    },
  };
  f.update({ connection: replacement });
  await f.ui.flush();
  assert.equal(f.requests.length, 2);
  f.requests[0].resolve(response("old connection"));
  await f.ui.flush();
  assert.equal(refresh(f.ui).props.disabled, true);
  refresh(f.ui).props.onClick();
  await f.ui.flush();
  assert.equal(f.requests.length, 2);
  f.requests[1].resolve(response("new connection"));
  await f.ui.flush();
  assert.ok(content(f.ui.tree).includes("new connection"));
  assert.ok(!content(f.ui.tree).includes("old connection"));
});

test("unmount retires saved actions and pending link error notifications", async (t) => {
  const f = await fixture(t);
  const opened = deferred();
  f.update({
    connection: {
      ...f.host.announcements.connection,
      openRelease(url) {
        f.calls.push({ command: "open", url });
        return opened.promise;
      },
    },
  });
  await f.ui.flush();
  f.requests.at(-1).resolve(response("visible"));
  await f.ui.flush();
  const savedRefresh = refresh(f.ui).props.onClick,
    savedLink = link(f.ui).props.onClick;
  savedLink();
  f.ui.close();
  const count = f.calls.length;
  savedRefresh();
  savedLink();
  opened.reject(new Error("late link error"));
  await f.ui.flush();
  assert.equal(f.calls.length, count);
  assert.deepEqual(f.notifications, []);
  f.requests[0].resolve(response("late data"));
});

test("preview and disabled announcement policy do not issue plugin reads", async (t) => {
  const f = await fixture(t);
  f.requests[0].resolve(response("initial"));
  await f.ui.flush();
  f.update({ available: false });
  await f.ui.flush();
  const count = f.calls.length;
  assert.equal(f.ui.tree, null);
  f.update({ available: true, scope: "none" });
  await f.ui.flush();
  assert.equal(f.ui.tree, null);
  assert.equal(f.calls.length, count);
  f.update({ scope: "all" });
  await f.ui.flush();
  assert.equal(f.calls.length, count + 1);
  f.requests.at(-1).resolve(response("enabled"));
  await f.ui.flush();
  assert.ok(content(f.ui.tree).includes("enabled"));
});

test("actual slot consumes registrations and withdraws UI and supported effects", async (t) => {
  const f = await fixture(t);
  f.requests[0].resolve(response("initial"));
  await f.ui.flush();
  const ui = harness(
    "LauncherSurfaceSlot",
    { port: f.port, slot: "home.announcements", host: f.host },
    { module: "productPlugins/LauncherSurfaceSlot.tsx" },
  );
  t.after(() => ui.close());
  const effects = harness(
    "useLauncherSurfaceEffects",
    { port: f.port },
    {
      module: "productPlugins/LauncherSurfaceSlot.tsx",
      render: (m, p) => m.useLauncherSurfaceEffects(p.port),
    },
  );
  t.after(() => effects.close());
  await ui.flush();
  await effects.flush();
  const first = elements(
    ui.tree,
    (n) => n.type?.name === "LauncherSurfaceBoundary",
  )[0];
  assert.ok(first);
  assert.equal(first.props.children.props.host, f.host);
  assert.deepEqual(effects.tree, ["announcements"]);
  await f.disable();
  await ui.flush();
  await effects.flush();
  assert.deepEqual(
    elements(ui.tree, (n) => n.type?.name === "LauncherSurfaceBoundary"),
    [],
  );
  assert.deepEqual(effects.tree, []);
  await f.enable();
  await ui.flush();
  await effects.flush();
  const next = elements(
    ui.tree,
    (n) => n.type?.name === "LauncherSurfaceBoundary",
  )[0];
  assert.ok(next);
  assert.notEqual(next.key, first.key);
  assert.equal(first.props.children.props.lifetime.isActive(), false);
  assert.deepEqual(effects.tree, ["announcements"]);
  f.requests.at(-1).resolve(response("enabled"));
});
