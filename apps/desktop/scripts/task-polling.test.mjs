/** Actual task hook, scheduler revisions and deterministic browser events.
 * Private synthetic replies only; never starts a desktop or a download. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url),
  ts = require("typescript");
const flush = () => new Promise((resolve) => setImmediate(resolve));
function deferred() {
  let resolve;
  return {
    promise: new Promise((yes) => {
      resolve = yes;
    }),
    resolve: (value) => resolve(value),
  };
}
function task(id, stage = "downloading") {
  return {
    task_id: id,
    stage,
    message: "fixture",
    version: "fixture",
    completed: 0,
    total: 1,
    bytes_done: 0,
    bytes_total: 1,
    root_id: "root",
    kind: "install",
  };
}
function view(revision = "1", tasks = []) {
  return {
    revision,
    tasks,
    blockedRootIds: [],
    runningLimit: 4,
    pendingLimit: 32,
  };
}
function driver(
  api,
  {
    native = true,
    subscribe = true,
    subscribeWait,
    onTerminal = () => {},
  } = {},
) {
  let hooks = [],
    cursor = 0,
    dirty = false,
    effects = [],
    result,
    props,
    now = 0,
    nextTimer = 1,
    writes = 0,
    closing;
  const timers = new Map(),
    taskEvents = new Set(),
    windowEvents = new Map(),
    documentEvents = new Map();
  function events(map) {
    return {
      addEventListener(type, callback) {
        if (!map.has(type)) map.set(type, new Set());
        map.get(type).add(callback);
      },
      removeEventListener(type, callback) {
        map.get(type)?.delete(callback);
      },
    };
  }
  const browser = {
    ...events(windowEvents),
    setTimeout(callback, delay) {
      const id = nextTimer++;
      timers.set(id, { callback, due: now + delay });
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
  };
  const document = { ...events(documentEvents), visibilityState: "visible" };
  const react = {
    useRef(value) {
      return (hooks[cursor++] ??= { current: value });
    },
    useState(initial) {
      const id = cursor++;
      if (!(id in hooks))
        hooks[id] = typeof initial === "function" ? initial() : initial;
      return [
        hooks[id],
        (next) => {
          const value = typeof next === "function" ? next(hooks[id]) : next;
          if (!Object.is(value, hooks[id])) {
            hooks[id] = value;
            dirty = true;
            writes++;
          }
        },
      ];
    },
    useCallback(fn, deps) {
      const id = cursor++,
        old = hooks[id];
      if (!old || deps.some((value, i) => !Object.is(value, old.deps[i])))
        hooks[id] = { value: fn, deps };
      return hooks[id].value;
    },
    useEffect(fn, deps) {
      const id = cursor++,
        old = hooks[id];
      if (!old || deps.some((value, i) => !Object.is(value, old.deps[i]))) {
        hooks[id] = { deps };
        effects.push(() => {
          old?.cleanup?.();
          hooks[id].cleanup = fn();
        });
      }
    },
  };
  const modules = new Map();
  function load(name) {
    if (modules.has(name)) return modules.get(name);
    const module = { exports: {} },
      source = readFileSync(
        new URL(`../src/${name}.ts`, import.meta.url),
        "utf8",
      );
    const code = ts.transpileModule(source, {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    }).outputText;
    function local(id) {
      if (id === "react") return react;
      if (id === "@tauri-apps/api/event")
        return {
          listen: async (_type, callback) => {
            if (!subscribe) throw Error("synthetic subscription unavailable");
            if (subscribeWait) await subscribeWait;
            taskEvents.add(callback);
            return () => taskEvents.delete(callback);
          },
        };
      if (id === "./i18n") return { t: (text) => text };
      if (id === "./useLauncherMinecraftNotices")
        return {
          createLauncherClosingSource: () => async (callback) => {
            closing = callback;
            return () => {
              if (closing === callback) closing = undefined;
            };
          },
        };
      return load(id.replace(/^\.\//, ""));
    }
    new Function("require", "module", "exports", "window", "document", code)(
      local,
      module,
      module.exports,
      browser,
      document,
    );
    modules.set(name, module.exports);
    return module.exports;
  }
  const { useDownloadTasks } = load("useDownloadTasks");
  props = { api, native, onTerminal };
  function render() {
    cursor = 0;
    dirty = false;
    effects = [];
    result = useDownloadTasks(props);
    effects.forEach((effect) => effect());
  }
  async function settle() {
    for (let n = 0; n < 10; n++) {
      await flush();
      if (dirty) render();
      else return;
    }
    throw Error("Hook did not settle");
  }
  render();
  return {
    settle,
    get result() {
      return result;
    },
    get writes() {
      return writes;
    },
    async tick(ms) {
      const until = now + ms;
      for (let n = 0; n < 1000; n++) {
        const next = [...timers]
          .filter(([, timer]) => timer.due <= until)
          .sort((a, b) => a[1].due - b[1].due)[0];
        if (!next) {
          now = until;
          return;
        }
        now = next[1].due;
        timers.delete(next[0]);
        next[1].callback();
        await settle();
      }
      throw Error("Unbounded timer loop");
    },
    event() {
      for (const callback of taskEvents)
        callback({ payload: { untrusted: true } });
    },
    async focus() {
      for (const callback of windowEvents.get("focus") || []) callback();
      await settle();
    },
    async close(value) {
      closing?.(value);
      await settle();
    },
    replace(next) {
      props = { ...props, ...next };
      render();
    },
    dispose() {
      for (const hook of hooks) hook?.cleanup?.();
    },
  };
}

test("idle native reads slow down and identical scheduler revisions do not update state", async (t) => {
  let reads = 0;
  const d = driver(async () => {
    reads++;
    return view();
  });
  await d.settle();
  const original = d.result.view,
    writes = d.writes;
  await d.tick(60_000);
  assert.ok(reads <= 6, `idle reads: ${reads}`);
  assert.equal(d.writes, writes);
  assert.equal(d.result.view, original);
  t.diagnostic(
    `synthetic idle 60s: task reads=${reads}, repeated state updates=${d.writes - writes}`,
  );
  const before = reads;
  await d.focus();
  assert.equal(reads, before + 1);
  d.dispose();
});

test("active work and unavailable event subscriptions retain the one-second fallback", async () => {
  for (const [native, subscribe, tasks] of [
    [true, true, [task("live")]],
    [true, false, []],
    [false, false, []],
  ]) {
    let reads = 0;
    const d = driver(
      async () => {
        reads++;
        return view("1", tasks);
      },
      { native, subscribe },
    );
    await d.settle();
    const initial = reads;
    await d.tick(5000);
    assert.equal(reads, initial + 5);
    d.dispose();
  }
});

test("one hundred progress hints coalesce without losing terminal membership or replaying callbacks", async () => {
  let reads = 0,
    current = view("1", [task("a"), task("b")]);
  const finished = [];
  const d = driver(
    async () => {
      reads++;
      return structuredClone(current);
    },
    { onTerminal: (value) => finished.push(value.task_id) },
  );
  await d.settle();
  const initial = reads;
  current = view("2", [task("a", "complete"), task("b", "cancelled")]);
  for (let i = 0; i < 100; i++) d.event();
  await d.tick(49);
  assert.equal(reads, initial);
  await d.tick(1);
  assert.equal(reads, initial + 1);
  assert.deepEqual(finished, ["a", "b"]);
  await d.tick(60_000);
  assert.deepEqual(finished, ["a", "b"]);
  d.dispose();
});

test("explicit refresh starts after its wake and physical reads stay serial", async () => {
  const pending = deferred();
  let reads = 0,
    active = 0,
    peak = 0;
  const d = driver(async () => {
    reads++;
    active++;
    peak = Math.max(peak, active);
    try {
      return reads === 1 ? await pending.promise : view("2");
    } finally {
      active--;
    }
  });
  await d.settle();
  const refreshed = d.result.refresh();
  for (let i = 0; i < 100; i++) d.event();
  await d.tick(5000);
  assert.equal(reads, 1);
  pending.resolve(view("1"));
  await d.settle();
  assert.equal((await refreshed).revision, "2");
  assert.equal(peak, 1);
  assert.equal(reads, 2);
  d.dispose();
});

test("owner replacement and closing retire late replies and pending event timers", async () => {
  const pending = deferred();
  let old = 0,
    fresh = 0;
  const d = driver(async () => {
    old++;
    return pending.promise;
  });
  await d.settle();
  d.replace({
    api: async () => {
      fresh++;
      return view("2");
    },
  });
  await d.settle();
  assert.equal(fresh, 0);
  pending.resolve(view("1", [task("old", "complete")]));
  await d.settle();
  assert.ok(fresh >= 1);
  assert.equal(d.result.view.revision, "2");
  const beforeClose = fresh;
  d.event();
  await d.close(true);
  await d.tick(60_000);
  assert.equal(fresh, beforeClose);
  await d.close(false);
  assert.equal(fresh, beforeClose + 1);
  assert.equal(old, 1);
  d.dispose();
});

test("subscription establishment fences an earlier empty read even if both lifecycle events were missed", async () => {
  const listening = deferred(),
    old = deferred();
  let reads = 0;
  const finished = [];
  const d = driver(
    async () =>
      ++reads === 1 ? old.promise : view("2", [task("missed", "complete")]),
    {
      subscribeWait: listening.promise,
      onTerminal: (value) => finished.push(value.task_id),
    },
  );
  await d.settle();
  assert.equal(reads, 1);
  listening.resolve();
  await d.settle();
  assert.equal(reads, 1);
  old.resolve(view("1"));
  await d.settle();
  assert.equal(reads, 2);
  assert.equal(d.result.view.revision, "2");
  assert.deepEqual(finished, ["missed"]);
  d.dispose();
});

test("root blocking still refreshes when scheduler revision is unchanged", async () => {
  let current = view("1", [task("done", "complete")]);
  const d = driver(async () => structuredClone(current));
  await d.settle();
  const tasks = d.result.view.tasks;
  current.blockedRootIds = ["retargeted-root"];
  await d.result.refresh();
  await d.settle();
  assert.deepEqual(d.result.view.blockedRootIds, ["retargeted-root"]);
  assert.equal(d.result.view.tasks, tasks);
  d.dispose();
});
