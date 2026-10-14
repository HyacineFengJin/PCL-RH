/** Real log-panel handlers and hooks, with synthetic rows and delayed replies.
 * No native commands, real logs, accounts, or game directories are accessed. */
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { elements, content, deferred } from "./maker-test-harness.mjs";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const source = fileURLToPath(new URL("../src/", import.meta.url));

// The shared Maker harness lacks useCallback and contexts. This local fixture
// supplies those hooks and keeps settings child panels as separate components.
function panel(name, initialProps) {
  const hooks = [],
    modules = new Map();
  let props = initialProps,
    cursor = 0,
    dirty = false,
    pending = [],
    tree;
  let closed = false,
    stateWritesAfterClose = 0;
  const react = {
    useState(initial) {
      const i = cursor++;
      if (!(i in hooks))
        hooks[i] = typeof initial === "function" ? initial() : initial;
      return [
        hooks[i],
        (next) => {
          if (closed) {
            stateWritesAfterClose++;
            return;
          }
          const value = typeof next === "function" ? next(hooks[i]) : next;
          if (!Object.is(value, hooks[i])) {
            hooks[i] = value;
            dirty = true;
          }
        },
      ];
    },
    useRef(initial) {
      return (hooks[cursor++] ??= { current: initial });
    },
    useMemo(factory, deps) {
      const i = cursor++,
        old = hooks[i];
      if (!old || deps.some((value, j) => !Object.is(value, old.deps[j])))
        hooks[i] = { deps, value: factory() };
      return hooks[i].value;
    },
    useCallback(callback, deps) {
      return react.useMemo(() => callback, deps);
    },
    useEffect(effect, deps) {
      const i = cursor++,
        old = hooks[i];
      if (!old || deps.some((value, j) => !Object.is(value, old.deps[j]))) {
        hooks[i] = { deps };
        pending.push(() => {
          old?.cleanup?.();
          hooks[i].cleanup = effect();
        });
      }
    },
    createContext(value) {
      const context = { value };
      context.Provider = { context };
      return context;
    },
    useContext(context) {
      return context.value;
    },
  };
  const controls = {
    LauncherCard: function LauncherCard() {},
    useLauncherPreferenceEditor: (p) => ({
      prefs: p.preferences?.preferences || {
        localization: { language: "zh-CN", region: "zh-CN" },
      },
    }),
  };
  function load(file) {
    if (modules.has(file)) return modules.get(file).exports;
    const module = { exports: {} };
    modules.set(file, module);
    const code = ts.transpileModule(fs.readFileSync(file, "utf8"), {
      fileName: file,
      compilerOptions: {
        target: ts.ScriptTarget.ES2022,
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.ReactJSX,
      },
    }).outputText;
    function localRequire(id) {
      if (id === "react") return react;
      if (id === "./LauncherSettingsControls") return controls;
      if (
        [
          "./CeSelect",
          "./LauncherPersonalize",
          "./LauncherMisc",
          "./LauncherUpdates",
        ].includes(id)
      )
        return {};
      if (id === "@tauri-apps/api/core") return { isTauri: () => false };
      if (id === "lucide-react") return {};
      if (/\.(png|jpe?g|ico|css)$/.test(id)) return id;
      if (id.startsWith(".")) {
        const base = path.resolve(path.dirname(file), id);
        return load(
          fs.existsSync(base + ".tsx") ? base + ".tsx" : base + ".ts",
        );
      }
      return require(id);
    }
    new Function("require", "module", "exports", code)(
      localRequire,
      module,
      module.exports,
    );
    return module.exports;
  }
  const component = load(path.join(source, `${name}.tsx`));
  const i18n = load(path.join(source, "i18n.ts"));
  function render() {
    cursor = 0;
    dirty = false;
    pending = [];
    tree = component[name](props);
    if (name === "ExtraSettings") {
      // Render its two providers and content function, then inspect the child
      // LauncherLogs props without merging that panel's independent hooks.
      for (let i = 0; i < 2; i++) {
        tree.type.context.value = tree.props.value;
        tree = tree.props.children;
      }
      tree = tree.type(tree.props);
    }
    for (const effect of pending) effect();
  }
  render();
  return {
    i18n,
    get tree() {
      return tree;
    },
    get stateWritesAfterClose() {
      return stateWritesAfterClose;
    },
    replace(next) {
      props = { ...props, ...next };
      render();
    },
    async flush() {
      for (let i = 0; i < 30; i++) {
        await new Promise((done) => setImmediate(done));
        if (closed || !dirty) return;
        render();
      }
      throw Error("Log panel did not settle");
    },
    close() {
      closed = true;
      for (const hook of hooks) hook?.cleanup?.();
    },
  };
}

const row = (name, current = false) => ({
  name,
  current,
  path: `/synthetic/logs/${name}`,
  modified: 1,
  size: 10,
});
const desired = (name = "failed.log", requestKey = "request-1") => ({
  name,
  requestKey,
});
function service() {
  const calls = [],
    lists = [],
    reads = [];
  const api = async (command, args) => {
    calls.push({ command, args });
    if (command === "launcher_logs") {
      const reply = deferred();
      lists.push(reply);
      return reply.promise;
    }
    if (command === "launcher_read_log") {
      const reply = deferred();
      reads.push({ ...reply, name: args.name });
      return reply.promise;
    }
    if (command === "launcher_log_recovery") return [];
    if (command === "launcher_prepare_log_export")
      return { revision: "synthetic-export" };
    if (command === "launcher_export_logs") return null;
    throw Error(`Unexpected synthetic command ${command}`);
  };
  return { api, calls, lists, reads };
}
function fixture(extra = {}) {
  const backend = service(),
    notices = [];
  const ui = panel("LauncherLogs", {
    api: backend.api,
    scopeKey: "root-a:/synthetic/root-a",
    native: true,
    operationsEnabled: true,
    busy: false,
    region: "zh-CN",
    language: "zh-CN",
    onOpen() {},
    onNotify: (message) => notices.push(message),
    ...extra,
  });
  return { ui, notices, ...backend };
}
function button(ui, key) {
  const node = elements(
    ui.tree,
    (value) => value.type === "button" && content(value) === ui.i18n.t(key),
  )[0];
  assert.ok(node, `Missing button ${key}`);
  return node;
}
function logButton(ui, name) {
  const node = elements(
    ui.tree,
    (value) =>
      value.type === "button" &&
      value.key === name &&
      value.props.className === "extra-log-row",
  )[0];
  assert.ok(node, `Missing log row ${name}`);
  return node;
}
const selected = (ui) =>
  elements(
    ui.tree,
    (value) =>
      value.props?.className === "extra-log-row" && value.props["aria-pressed"],
  ).map((value) => value.key);
const logContent = (ui) =>
  elements(ui.tree, (value) => value.type === "pre").map(content);

test("failure entry waits for rows and reads/exports the exact named log", async () => {
  const f = fixture({ initialLog: desired() });
  await f.ui.flush();
  assert.equal(f.reads.length, 0);
  f.lists[0].resolve([row("current.log", true), row("failed.log")]);
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), ["failed.log"]);
  assert.deepEqual(
    f.reads.map((r) => r.name),
    ["failed.log"],
  );
  f.reads[0].resolve("synthetic failure content");
  await f.ui.flush();
  assert.deepEqual(logContent(f.ui), ["synthetic failure content"]);
  button(f.ui, "logs.export").props.onClick();
  await f.ui.flush();
  assert.deepEqual(
    f.calls.filter((c) => c.command.includes("export")),
    [
      {
        command: "launcher_prepare_log_export",
        args: { selection: { kind: "named", name: "failed.log" } },
      },
      {
        command: "launcher_export_logs",
        args: {
          selection: { kind: "named", name: "failed.log" },
          revision: "synthetic-export",
        },
      },
    ],
  );
  f.ui.close();
});

test("missing exact log warns without current fallback and allows manual selection", async () => {
  const f = fixture({ initialLog: desired("missing.log") });
  f.lists[0].resolve([row("current.log", true), row("manual.log")]);
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), []);
  assert.equal(f.reads.length, 0);
  assert.ok(
    content(f.ui.tree).includes(
      f.ui.i18n.t("logs.requestedMissing", { name: "missing.log" }),
    ),
  );
  assert.equal(button(f.ui, "logs.export").props.disabled, true);
  button(f.ui, "logs.export").props.onClick();
  await f.ui.flush();
  assert.equal(f.calls.filter((c) => c.command.includes("export")).length, 0);
  button(f.ui, "logs.refresh").props.onClick();
  f.lists[1].resolve([row("current.log", true), row("manual.log")]);
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), []);
  logButton(f.ui, "manual.log").props.onClick();
  f.reads[0].resolve("manual content");
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), ["manual.log"]);
  assert.equal(button(f.ui, "logs.export").props.disabled, false);
  assert.equal(elements(f.ui.tree, (n) => n.props?.role === "alert").length, 0);
  button(f.ui, "logs.refresh").props.onClick();
  f.lists[2].resolve([
    row("missing.log"),
    row("manual.log"),
    row("current.log", true),
  ]);
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), ["manual.log"]);
  assert.equal(f.reads.length, 1);
  f.ui.close();
});

test("refresh preserves manual choice and only a new request reselects the same name", async () => {
  const f = fixture({ initialLog: desired() });
  const rows = [row("failed.log"), row("manual.log"), row("current.log", true)];
  f.lists[0].resolve(rows);
  await f.ui.flush();
  logButton(f.ui, "manual.log").props.onClick();
  await f.ui.flush();
  f.reads[1].resolve("manual content");
  f.reads[0].resolve("late failure content");
  await f.ui.flush();
  assert.deepEqual(logContent(f.ui), ["manual content"]);
  button(f.ui, "logs.refresh").props.onClick();
  f.lists[1].resolve(rows);
  await f.ui.flush();
  f.ui.replace({ initialLog: desired() });
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), ["manual.log"]);
  assert.equal(f.reads.length, 2);
  f.ui.replace({ initialLog: desired("failed.log", "request-2") });
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), ["failed.log"]);
  assert.equal(f.reads.length, 3);
  f.ui.close();
});

test("a newer entry request retires both a late read and saved old read handlers", async () => {
  const f = fixture({ initialLog: desired() });
  f.lists[0].resolve([row("failed.log"), row("second.log")]);
  await f.ui.flush();
  const oldRead = logButton(f.ui, "failed.log").props.onClick;
  f.ui.replace({ initialLog: desired("second.log", "request-2") });
  await f.ui.flush();
  oldRead();
  await f.ui.flush();
  assert.equal(f.reads.length, 2);
  f.reads[1].resolve("second request content");
  f.reads[0].resolve("old request content");
  await f.ui.flush();
  assert.deepEqual(logContent(f.ui), ["second request content"]);
  assert.deepEqual(selected(f.ui), ["second.log"]);
  f.ui.close();
});

test("request replacement while a list is pending reads only the newest exact name", async () => {
  const f = fixture({ initialLog: desired() });
  f.ui.replace({ initialLog: desired("second.log", "request-2") });
  await f.ui.flush();
  f.lists[0].resolve([
    row("failed.log"),
    row("second.log"),
    row("current.log", true),
  ]);
  await f.ui.flush();
  assert.deepEqual(
    f.reads.map((r) => r.name),
    ["second.log"],
  );
  f.ui.close();
});

test("root changes retire read replies even when the new root has the same filename", async () => {
  const f = fixture({ initialLog: desired() });
  f.lists[0].resolve([row("failed.log")]);
  await f.ui.flush();
  f.ui.replace({ scopeKey: "root-b:/synthetic/root-b" });
  await f.ui.flush();
  f.lists[1].resolve([row("failed.log")]);
  await f.ui.flush();
  f.reads[1].resolve("new root content");
  f.reads[0].resolve("old root content");
  await f.ui.flush();
  assert.deepEqual(logContent(f.ui), ["new root content"]);
  f.ui.close();
});

test("replacement APIs retire reads and failures within the same root", async () => {
  const f = fixture({ initialLog: desired() }),
    replacement = service();
  f.lists[0].resolve([row("failed.log")]);
  await f.ui.flush();
  f.ui.replace({ api: replacement.api });
  await f.ui.flush();
  replacement.lists[0].resolve([row("failed.log")]);
  await f.ui.flush();
  replacement.reads[0].resolve("replacement API content");
  f.reads[0].reject(Error("old API failure"));
  await f.ui.flush();
  assert.deepEqual(logContent(f.ui), ["replacement API content"]);
  assert.deepEqual(f.notices, []);
  f.ui.close();
});

test("an obsolete list cannot select or read through an old API", async () => {
  const f = fixture({ initialLog: desired() }),
    replacement = service();
  const oldRefresh = button(f.ui, "logs.refresh").props.onClick;
  f.ui.replace({ api: replacement.api });
  await f.ui.flush();
  replacement.lists[0].resolve([row("failed.log")]);
  await f.ui.flush();
  f.lists[0].resolve([row("failed.log"), row("old-only.log", true)]);
  oldRefresh();
  await f.ui.flush();
  assert.equal(f.reads.length, 0);
  assert.equal(f.lists.length, 1);
  assert.equal(replacement.reads.length, 1);
  assert.equal(elements(f.ui.tree, (n) => n.key === "old-only.log").length, 0);
  f.ui.close();
});

test("unmount retires both read success and failure without state writes or notices", async () => {
  for (const fail of [false, true]) {
    const f = fixture({ initialLog: desired() });
    f.lists[0].resolve([row("failed.log")]);
    await f.ui.flush();
    f.ui.close();
    if (fail) f.reads[0].reject(Error("late unmounted failure"));
    else f.reads[0].resolve("late unmounted text");
    await f.ui.flush();
    assert.equal(f.ui.stateWritesAfterClose, 0);
    assert.deepEqual(f.notices, []);
  }
});

test("an entry-selected log removed by refresh becomes explicitly missing instead of current", async () => {
  const f = fixture({ initialLog: desired() });
  f.lists[0].resolve([row("failed.log"), row("current.log", true)]);
  await f.ui.flush();
  button(f.ui, "logs.refresh").props.onClick();
  f.lists[1].resolve([row("current.log", true)]);
  await f.ui.flush();
  f.reads[0].resolve("late removed failure content");
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), []);
  assert.deepEqual(logContent(f.ui), []);
  assert.equal(button(f.ui, "logs.export").props.disabled, true);
  assert.ok(
    content(f.ui.tree).includes(
      f.ui.i18n.t("logs.requestedMissing", { name: "failed.log" }),
    ),
  );
  logButton(f.ui, "current.log").props.onClick();
  f.reads[1].resolve("manually chosen current content");
  await f.ui.flush();
  assert.deepEqual(logContent(f.ui), ["manually chosen current content"]);
  assert.equal(button(f.ui, "logs.export").props.disabled, false);
  f.ui.close();
});

test("a prior entry read failure stays silent after a newer request succeeds", async () => {
  const f = fixture({ initialLog: desired() });
  f.lists[0].resolve([row("failed.log")]);
  await f.ui.flush();
  f.ui.replace({ initialLog: desired("failed.log", "request-2") });
  await f.ui.flush();
  f.reads[1].resolve("current request content");
  f.reads[0].reject(Error("obsolete request failure"));
  await f.ui.flush();
  assert.deepEqual(logContent(f.ui), ["current request content"]);
  assert.deepEqual(f.notices, []);
  f.ui.close();
});

test("normal list refresh still keeps a selected row then defaults to current if removed", async () => {
  const f = fixture();
  f.lists[0].resolve([row("manual.log"), row("current.log", true)]);
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), ["current.log"]);
  assert.equal(f.reads.length, 0);
  logButton(f.ui, "manual.log").props.onClick();
  await f.ui.flush();
  button(f.ui, "logs.refresh").props.onClick();
  f.lists[1].resolve([row("manual.log"), row("next-current.log", true)]);
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), ["manual.log"]);
  button(f.ui, "logs.refresh").props.onClick();
  f.lists[2].resolve([row("next-current.log", true)]);
  await f.ui.flush();
  f.reads[0].resolve("content of removed manual log");
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), ["next-current.log"]);
  assert.deepEqual(logContent(f.ui), []);
  f.ui.close();
});

test("a missing entry becomes readable when refresh finds its exact log for the same request", async () => {
  const f = fixture({ initialLog: desired() });
  f.lists[0].resolve([row("current.log", true)]);
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), []);
  assert.equal(button(f.ui, "logs.export").props.disabled, true);
  button(f.ui, "logs.refresh").props.onClick();
  f.lists[1].resolve([row("current.log", true)]);
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), []);
  assert.equal(f.reads.length, 0);
  button(f.ui, "logs.refresh").props.onClick();
  f.lists[2].resolve([
    row("current.log", true),
    row("failed.log"),
    row("manual.log"),
  ]);
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), ["failed.log"]);
  assert.deepEqual(
    f.reads.map((r) => r.name),
    ["failed.log"],
  );
  assert.equal(elements(f.ui.tree, (n) => n.props?.role === "alert").length, 0);
  f.reads[0].resolve("recovered exact failure content");
  await f.ui.flush();
  assert.deepEqual(logContent(f.ui), ["recovered exact failure content"]);
  assert.equal(button(f.ui, "logs.export").props.disabled, false);
  logButton(f.ui, "manual.log").props.onClick();
  await f.ui.flush();
  button(f.ui, "logs.refresh").props.onClick();
  f.lists[3].resolve([
    row("failed.log"),
    row("manual.log"),
    row("current.log", true),
  ]);
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), ["manual.log"]);
  assert.equal(f.reads.length, 2);
  f.ui.close();
});

test("a new request key for a missing entry stays explicit until the exact log appears", async () => {
  const f = fixture({ initialLog: desired() });
  f.lists[0].resolve([row("current.log", true)]);
  await f.ui.flush();
  f.ui.replace({ initialLog: desired("failed.log", "retry") });
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), []);
  assert.equal(f.reads.length, 0);
  assert.equal(button(f.ui, "logs.export").props.disabled, true);
  button(f.ui, "logs.refresh").props.onClick();
  f.lists[1].resolve([row("failed.log"), row("current.log", true)]);
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), ["failed.log"]);
  assert.deepEqual(
    f.reads.map((r) => r.name),
    ["failed.log"],
  );
  assert.equal(elements(f.ui.tree, (n) => n.props?.role === "alert").length, 0);
  f.ui.close();
});

test("the newest list refresh owns rows and leaves a consumed entry unchanged", async () => {
  const f = fixture({ initialLog: desired() });
  f.lists[0].resolve([row("failed.log")]);
  await f.ui.flush();
  button(f.ui, "logs.refresh").props.onClick();
  button(f.ui, "logs.refresh").props.onClick();
  f.lists[2].resolve([row("failed.log"), row("new-list.log")]);
  await f.ui.flush();
  f.lists[1].resolve([row("old-list.log", true)]);
  await f.ui.flush();
  assert.deepEqual(selected(f.ui), ["failed.log"]);
  assert.equal(f.reads.length, 1);
  assert.ok(logButton(f.ui, "new-list.log"));
  assert.equal(elements(f.ui.tree, (n) => n.key === "old-list.log").length, 0);
  f.ui.close();
});

test("ExtraSettings forwards the entry selection to the existing root-bound logs panel", async () => {
  const backend = service(),
    rootBackend = service(),
    initialLog = desired();
  const ui = panel("ExtraSettings", {
    section: "logs",
    settings: { root_id: "root-a", root: "/synthetic/root-a" },
    api: backend.api,
    rootApi: rootBackend.api,
    native: true,
    initialLog,
    supportedEffects: ["logs"],
    onOpen() {},
    onNotify() {},
  });
  await ui.flush();
  assert.equal(ui.tree.type.name, "LauncherLogs");
  assert.equal(ui.tree.props.initialLog, initialLog);
  assert.equal(ui.tree.props.api, rootBackend.api);
  assert.equal(ui.tree.props.scopeKey, "root-a:/synthetic/root-a");
  assert.equal(ui.tree.key, "root-a:/synthetic/root-a");
  assert.equal(backend.calls.length, 0);
  assert.equal(rootBackend.calls.length, 0);
  ui.close();
});
