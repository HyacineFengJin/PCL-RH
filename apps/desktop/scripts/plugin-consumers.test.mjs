/** A real homepage hook retires visible and in-flight plugin contributions. */
import assert from "node:assert/strict";
import test from "node:test";
import { harness, elements, content, deferred } from "./maker-test-harness.mjs";
function fixture() {
  const requests = [],
    calls = [];
  const props = {
    native: true,
    enabled: true,
    pluginEnabled: true,
    contextKey: "home",
    canNavigate: () => true,
    onResource() {},
    onNotify() {},
    launcher: {
      prefs: {
        localization: { language: "en-US", region: "US" },
        announcement_mode: "all",
        network: {},
        management: { clipboard_resource_detection: false },
      },
      view: { revision: "fixture" },
      importEpoch: 0,
    },
    api: async (command, args) => {
      calls.push({ command, args });
      const d = deferred();
      requests.push(d);
      return d.promise;
    },
  };
  const ui = harness("useLauncherDiscovery", props, {
    module: "useLauncherDiscovery.tsx",
    render: (m, p) => m.useLauncherDiscovery(p).announcementUi,
  });
  return { ui, requests, calls };
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
test("disable removes cached content and rejects responses from the previous plugin activation", async (t) => {
  const f = fixture();
  t.after(() => f.ui.close());
  await f.ui.flush();
  f.ui.replace({ pluginEnabled: false });
  f.requests[0].resolve(response("retired reply"));
  await f.ui.flush();
  assert.equal(f.ui.tree, null);
  f.ui.replace({ pluginEnabled: true });
  await f.ui.flush();
  assert.equal(f.requests.length, 2);
  f.requests[1].resolve(response("current reply"));
  await f.ui.flush();
  assert.ok(content(f.ui.tree).includes("current reply"));
  assert.ok(!content(f.ui.tree).includes("retired reply"));
  f.ui.replace({ pluginEnabled: false });
  assert.equal(f.ui.tree, null);
});
test("a saved refresh callback stays retired after disabling and reenabling the plugin", async (t) => {
  const f = fixture();
  t.after(() => f.ui.close());
  await f.ui.flush();
  f.requests[0].resolve(response("initial"));
  await f.ui.flush();
  const refresh = elements(
    f.ui.tree,
    (n) =>
      n.type === "button" && /Refresh announcements|刷新公告/.test(content(n)),
  )[0];
  assert.ok(refresh, "the actual announcement refresh button is rendered");
  const saved = refresh.props.onClick;
  const savedLink = elements(
    f.ui.tree,
    (n) => n.type === "button" && n !== refresh,
  )[0].props.onClick;
  f.ui.replace({ pluginEnabled: false });
  f.ui.replace({ pluginEnabled: true });
  await f.ui.flush();
  f.requests[1].resolve(response("new activation"));
  await f.ui.flush();
  const before = f.calls.length;
  saved();
  savedLink();
  await f.ui.flush();
  assert.equal(f.calls.length, before);
});
