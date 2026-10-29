/** Real discovery entry, activation lifetimes and component effects. All
 * clipboard replies are synthetic; no native clipboard or network is used. */
import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { harness, deferred } from "./maker-test-harness.mjs";
import {
  assemblePlugins,
  own,
  clipboardEntry,
  createClipboardDiscoveryConnection,
  createLauncherSurfacePort,
  plugin,
} from "./launcher-surface-fixture.mjs";
const manifest = JSON.parse(
  await readFile(
    new URL("../../../code/plugins.json", import.meta.url),
    "utf8",
  ),
).find((entry) => entry.id === clipboardEntry.id);
const link = (id = "example") => ({
  kind: "mod",
  projectIdOrSlug: id,
  url: `https://modrinth.com/mod/${id}`,
});

async function fixture(t, initial = {}) {
  const calls = [],
    requests = [],
    opened = [],
    notifications = [];
  const kernel = own(t, assemblePlugins([manifest], [clipboardEntry]));
  await kernel.configure([], ["launcher"]);
  const port = createLauncherSurfacePort();
  t.after(port.attach(kernel));
  let focused = true,
    navigable = true;
  const connection = {
    read() {
      const p = deferred();
      requests.push(p);
      calls.push("read");
      return p.promise;
    },
    isFocused: () => focused,
    canNavigate: () => navigable,
    open: (value) => opened.push(value),
  };
  let channel = {
    connection,
    enabled: true,
    available: true,
    context: "page-one",
    localization: { language: "en-US", region: "US" },
    notify: (value) => notifications.push(value),
    ...initial,
  };
  const host = () => ({
    announcements: { localization: channel.localization },
    clipboard: channel,
  });
  const ui = harness(
    "ClipboardDiscoverySurface",
    {
      host: host(),
      lifetime: port.list("launcher.discovery")[0].lifetime,
    },
    { module: "plugins/discovery/ClipboardDiscoverySurface.tsx" },
  );
  t.after(() => ui.close());
  return {
    ui,
    kernel,
    port,
    calls,
    requests,
    opened,
    notifications,
    focus: (value) => {
      focused = value;
    },
    navigate: (value) => {
      navigable = value;
    },
    update(patch) {
      channel = { ...channel, ...patch };
      ui.replace({ host: host() });
    },
    renew() {
      ui.replace({ lifetime: port.list("launcher.discovery")[0].lifetime });
    },
  };
}

test("discovery is a lazy launcher-only entry with a declared native service", async (t) => {
  let imports = 0,
    activated = 0;
  const kernel = own(
    t,
    assemblePlugins(
      [manifest],
      {
        [manifest.id]: [
          async () => {
            imports++;
            return clipboardEntry;
          },
        ],
      },
      {
        nativeServices: {
          activate: async (id) => {
            assert.equal(id, manifest.id);
            activated++;
          },
        },
      },
    ),
  );
  await kernel.configure([manifest.id], ["launcher"]);
  assert.equal(imports, 0);
  await kernel.configure([], ["launcher"]);
  assert.equal(imports, 1);
  assert.equal(activated, 1);
  assert.equal(manifest.nativeServices[0].id, "launcher.clipboard-discovery");
  const record = kernel.listLauncherSurfaces("launcher.discovery")[0];
  await kernel.configure([], ["launcher", "rh-code"]);
  await kernel.configure([], ["launcher"]);
  assert.equal(kernel.listLauncherSurfaces("launcher.discovery")[0], record);
});
test("discovery slot rejects a missing clipboard capability before entry import", async (t) => {
  let imports = 0;
  const kernel = own(
    t,
    assemblePlugins([{ ...manifest, capabilities: [] }], {
      [manifest.id]: [
        async () => {
          imports++;
          return clipboardEntry;
        },
      ],
    }),
  );
  await kernel.configure([], ["launcher"]);
  assert.equal(imports, 0);
  assert.equal(kernel.statuses.get(manifest.id).state, "failed");
});
test("settings support tracks both plugin effects independently", async (t) => {
  const definitions = assemblePlugins([manifest], [clipboardEntry]);
  const kernel = own(t, [...definitions, plugin("announcements")]);
  await kernel.configure([], ["launcher"]);
  const port = createLauncherSurfacePort();
  t.after(port.attach(kernel));
  const ui = harness(
    "useLauncherSurfaceEffects",
    { port },
    {
      module: "productPlugins/LauncherSurfaceSlot.tsx",
      render: (module, props) => module.useLauncherSurfaceEffects(props.port),
    },
  );
  t.after(() => ui.close());
  assert.deepEqual(ui.tree, ["announcements", "clipboard_detection"]);
  await kernel.configure([manifest.id], ["launcher"]);
  await ui.flush();
  assert.deepEqual(ui.tree, ["announcements"]);
  await kernel.configure(["announcements"], ["launcher"]);
  await ui.flush();
  assert.deepEqual(ui.tree, ["clipboard_detection"]);
  await kernel.configure([manifest.id, "announcements"], ["launcher"]);
  await ui.flush();
  assert.deepEqual(ui.tree, []);
});

test("a disabled preference creates no listeners, timer, read or visible UI", async (t) => {
  const f = await fixture(t, { enabled: false });
  assert.equal(f.ui.tree, null);
  assert.equal(f.ui.listenerCount("focus"), 0);
  assert.equal(f.ui.timerCount(), 0);
  await f.ui.poll();
  await f.ui.dispatch("focus");
  assert.equal(f.calls.length, 0);
  f.update({ enabled: true });
  assert.equal(f.calls.length, 1);
  assert.equal(f.ui.listenerCount("focus"), 1);
});
test("focus and navigation are checked both before read and before navigation", async (t) => {
  const f = await fixture(t);
  f.focus(false);
  f.requests[0].resolve(link());
  await f.ui.flush();
  assert.equal(f.opened.length, 0);
  await f.ui.poll();
  assert.equal(f.calls.length, 1);
  f.focus(true);
  f.navigate(false);
  await f.ui.dispatch("focus");
  assert.equal(f.calls.length, 1);
  f.navigate(true);
  await f.ui.dispatch("focus");
  assert.equal(f.calls.length, 2);
  f.navigate(false);
  f.requests[1].resolve(link());
  await f.ui.flush();
  assert.equal(f.opened.length, 0);
});
test("focus bursts stay single-flight and repeated links open only once", async (t) => {
  const f = await fixture(t);
  await f.ui.poll();
  await f.ui.dispatch("focus");
  await f.ui.poll();
  assert.equal(f.calls.length, 1);
  f.requests[0].resolve(link());
  await f.ui.flush();
  await f.ui.poll();
  f.requests[1].resolve(link());
  await f.ui.flush();
  assert.deepEqual(f.opened, [link()]);
  await f.ui.poll();
  f.requests[2].resolve(link("another"));
  await f.ui.flush();
  assert.deepEqual(f.opened, [link(), link("another")]);
});
test("navigation retires an old reply while preserving deduplication across pages", async (t) => {
  const f = await fixture(t);
  f.update({ context: "page-two" });
  f.requests[0].resolve(link());
  await f.ui.flush();
  assert.equal(f.opened.length, 0);
  await f.ui.poll();
  f.requests[1].resolve(link());
  await f.ui.flush();
  assert.equal(f.opened.length, 1);
  f.update({ context: "page-three" });
  f.requests[2].resolve(link());
  await f.ui.flush();
  assert.equal(f.opened.length, 1);
});
test("preference disable and reenable cannot revive the pending read", async (t) => {
  const f = await fixture(t);
  f.update({ enabled: false });
  assert.equal(f.ui.listenerCount("focus"), 0);
  assert.equal(f.ui.timerCount(), 0);
  f.update({ enabled: true });
  f.requests[0].resolve(link());
  await f.ui.flush();
  assert.equal(f.opened.length, 0);
  await f.ui.poll();
  f.requests[1].resolve(link());
  await f.ui.flush();
  assert.equal(f.opened.length, 1);
});
test("plugin retirement immediately blocks reads and replies before React unmounts", async (t) => {
  const f = await fixture(t);
  const saved = f.port.list("launcher.discovery")[0];
  await f.kernel.configure([manifest.id], ["launcher"]);
  assert.equal(saved.lifetime.isActive(), false);
  assert.deepEqual(f.port.effects(), []);
  f.requests[0].resolve(link());
  await f.ui.flush();
  await f.ui.poll();
  await f.ui.dispatch("focus");
  assert.equal(f.opened.length, 0);
  assert.equal(f.calls.length, 1);
  await f.kernel.configure([], ["launcher"]);
  f.renew();
  assert.equal(saved.lifetime.isActive(), false);
  assert.deepEqual(f.port.effects(), ["clipboard_detection"]);
  f.requests[1].resolve(link());
  await f.ui.flush();
  assert.equal(f.opened.length, 1);
});
test("host detachment retires an otherwise active discovery activation", async (t) => {
  const f = await fixture(t);
  const replacement = createLauncherSurfacePort();
  const release = f.port.attach(f.kernel);
  release();
  t.after(replacement.attach(f.kernel));
  f.requests[0].resolve(link());
  await f.ui.flush();
  await f.ui.poll();
  assert.equal(f.opened.length, 0);
  assert.equal(f.calls.length, 1);
});
test("unmount cleans subscriptions and suppresses a late rejection", async (t) => {
  const f = await fixture(t);
  f.ui.close();
  assert.equal(f.ui.listenerCount("focus"), 0);
  assert.equal(f.ui.timerCount(), 0);
  f.requests[0].reject(new Error("late fixture failure"));
  await f.ui.flush();
  assert.equal(f.notifications.length, 0);
});
test("only errors from the current available activation notify", async (t) => {
  const f = await fixture(t);
  f.update({ available: false });
  f.requests[0].reject(new Error("old fixture failure"));
  await f.ui.flush();
  assert.equal(f.notifications.length, 0);
  f.update({ available: true });
  f.requests[1].reject(new Error("current fixture failure"));
  await f.ui.flush();
  assert.equal(f.notifications.length, 1);
});
test("narrow native port rejects stale actions and late results", async () => {
  let current = true,
    focused = true,
    navigable = true,
    reads = 0;
  const pending = deferred(),
    opened = [];
  const port = createClipboardDiscoveryConnection({
    api: async (command, args) => {
      assert.equal(command, "launcher_clipboard_link");
      assert.equal(args, undefined);
      reads++;
      return pending.promise;
    },
    isCurrent: () => current,
    isFocused: () => focused,
    canNavigate: () => navigable,
    onResource: (value) => opened.push(value),
  });
  focused = false;
  assert.equal(await port.read(), null);
  focused = true;
  navigable = false;
  assert.equal(await port.read(), null);
  navigable = true;
  const request = port.read();
  current = false;
  pending.resolve(link());
  assert.equal(await request, null);
  port.open(link());
  assert.equal(await port.read(), null);
  assert.equal(reads, 1);
  assert.deepEqual(opened, []);
});
test("host binding replacement is monotonic even when a context value returns", async () => {
  const requests = [],
    opened = [];
  const binding = {
    api: async () => {
      const p = deferred();
      requests.push(p);
      return p.promise;
    },
    native: true,
    enabled: true,
    contextKey: "first",
    canNavigate: () => true,
    onResource: (value) => opened.push(value),
  };
  const ui = harness(
    "useClipboardDiscoveryHost",
    {
      host: {
        announcements: {
          localization: { language: "en-US", region: "US" },
          notify() {},
        },
      },
      binding,
    },
    {
      module: "productPlugins/clipboardDiscovery.ts",
      render: (module, props) =>
        module.useClipboardDiscoveryHost(props.host, props.binding),
    },
  );
  try {
    const old = ui.tree.clipboard.connection,
      pending = old.read();
    ui.replace({ binding: { ...binding, contextKey: "second" } });
    ui.replace({ binding: { ...binding } });
    requests[0].resolve(link());
    assert.equal(await pending, null);
    old.open(link());
    assert.deepEqual(opened, []);
    const current = ui.tree.clipboard.connection;
    current.open(link());
    assert.deepEqual(opened, [link()]);
    ui.close();
    current.open(link("retired"));
    assert.equal(opened.length, 1);
  } finally {
    ui.close();
  }
});
