/** Exercise the actual launcher host with deterministic hooks, clock, imports,
 * plugin ownership and native responses. No DOM, native store or provider runs. */
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
const require = createRequire(import.meta.url),
  ts = require("typescript");
const source = fileURLToPath(new URL("../src/", import.meta.url));
const shipped = JSON.parse(
  readFileSync(new URL("../../../code/plugins.json", import.meta.url), "utf8"),
);
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

function hostDriver(options = {}) {
  const hooks = [],
    modules = new Map(),
    timers = new Map();
  const calls = [],
    imports = [],
    configurations = [],
    controllers = [],
    kernels = [];
  const manifests = options.manifests ?? shipped;
  let cursor = 0,
    dirty = false,
    pending = [],
    now = 1_000,
    output,
    closed = false,
    child = null,
    childProps = null,
    dialog = false,
    acquire,
    props;
  const nativeResponses = [],
    prepareResponses = [];
  const root = { querySelector: () => (dialog ? {} : null) };
  const react = {
    createContext(initial) {
      return { initial, Provider: Symbol("Provider") };
    },
    useContext(context) {
      return context.initial;
    },
    useState(initial) {
      const index = cursor++;
      if (!(index in hooks))
        hooks[index] = typeof initial === "function" ? initial() : initial;
      return [
        hooks[index],
        (next) => {
          const result = typeof next === "function" ? next(hooks[index]) : next;
          if (!Object.is(result, hooks[index])) {
            hooks[index] = result;
            dirty = true;
          }
        },
      ];
    },
    useRef(initial) {
      return (hooks[cursor++] ??= { current: initial });
    },
    useCallback(callback, deps) {
      const index = cursor++,
        previous = hooks[index];
      if (
        !previous ||
        deps.some((item, index) => !Object.is(item, previous.deps[index]))
      )
        hooks[index] = { deps, callback };
      return hooks[index].callback;
    },
    useEffect(effect, deps) {
      const index = cursor++,
        previous = hooks[index];
      if (
        !previous ||
        !deps ||
        deps.some((item, index) => !Object.is(item, previous.deps[index]))
      ) {
        hooks[index] = { deps };
        pending.push(() => {
          previous?.cleanup?.();
          hooks[index].cleanup = effect();
        });
      }
    },
  };
  const jsx = (type, props) => ({ type, props });
  class Kernel {
    constructor(definitions) {
      this.plugins = definitions;
      this.statuses = new Map(
        definitions.map((p) => [p.manifest.id, { state: "dormant" }]),
      );
      this.disposed = false;
      kernels.push(this);
    }
    async configure(disabled, scopes) {
      configurations.push({ disabled: [...disabled], scopes: [...scopes] });
      for (const p of this.plugins)
        this.statuses.set(p.manifest.id, {
          state:
            scopes.includes(p.manifest.scope ?? "rh-code") ||
            p.manifest.scope === "both"
              ? "active"
              : "dormant",
        });
    }
    async dispose() {
      this.disposed = true;
    }
  }
  function View() {}
  async function api(command, args) {
    calls.push({ command, args });
    if (command === "rh_code_call" && args.operation === "bootstrap")
      return {
        revision: "synthetic-revision",
        disabledPlugins: [],
        projects: [{ id: "saved-project" }],
      };
    if (command === "experimental_suspend") {
      const response = nativeResponses.shift();
      return response ? await response : { released: true, busy: false };
    }
    throw new Error(`Unexpected synthetic command: ${command}`);
  }
  function newController() {
    const state = {
      busy: 0,
      dirty: false,
      activeJobs: 0,
      lastActivityAt: now,
      suspended: false,
    };
    const controller = {
      state,
      prepares: 0,
      restored: null,
      getLifecycleState: () => ({ ...state }),
      async prepareSuspend(stillInactive) {
        this.prepares++;
        const response = prepareResponses.shift();
        if (response) {
          const success = await response;
          if (!success || !stillInactive()) return false;
        }
        if (!stillInactive() || state.busy || state.activeJobs) return false;
        state.dirty = false;
        state.suspended = true;
        return true;
      },
    };
    controllers.push(controller);
    // The real RHCode mounts a fresh controller which bootstraps its saved data.
    void api("rh_code_call", { operation: "bootstrap", args: {} }).then(
      (snapshot) => {
        controller.restored = snapshot.projects[0].id;
      },
    );
    return controller;
  }
  function load(file) {
    if (modules.has(file)) return modules.get(file).exports;
    const module = { exports: {} };
    modules.set(file, module);
    const code = ts.transpileModule(readFileSync(file, "utf8"), {
      fileName: file,
      compilerOptions: {
        target: ts.ScriptTarget.ES2022,
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.ReactJSX,
      },
    }).outputText;
    const localRequire = (id) => {
      if (id === "react") return react;
      if (id === "react/jsx-runtime")
        return { jsx, jsxs: jsx, Fragment: Symbol("Fragment") };
      if (id.endsWith("plugins.json")) return manifests;
      if (id === "./i18n" || id === "../i18n")
        return { t: (id) => id, currentLocale: () => ({ language: "en-US" }) };
      if (id === "./rh-code/kernel") {
        imports.push("kernel");
        return { Kernel };
      }
      if (id === "./rh-code/plugins") {
        imports.push("plugins");
        return {
          productPlugins: () => manifests.map((manifest) => ({ manifest })),
        };
      }
      if (id === "./rh-code/RHCode") {
        imports.push("RHCode");
        return { RHCode: View };
      }
      if (id.startsWith(".")) {
        const target = path.resolve(path.dirname(file), id);
        return load(target + (id.endsWith("retention") ? ".tsx" : ".ts"));
      }
      return require(id);
    };
    class Clock extends Date {
      static now() {
        return now;
      }
    }
    new Function(
      "require",
      "module",
      "exports",
      "setInterval",
      "clearInterval",
      "Date",
      code,
    )(
      localRequire,
      module,
      module.exports,
      (callback) => {
        const id = Symbol("timer");
        timers.set(id, callback);
        return id;
      },
      (id) => timers.delete(id),
      Clock,
    );
    return module.exports;
  }
  const { RHCodeHost } = load(path.join(source, "RHCodeHost.tsx"));
  const retention = load(path.join(source, "rh-code/retention.tsx"));
  function walk(node, visitor) {
    if (!node || typeof node !== "object") return;
    if (Array.isArray(node)) {
      for (const child of node) walk(child, visitor);
      return;
    }
    visitor(node);
    walk(node.props?.children, visitor);
  }
  function render() {
    cursor = 0;
    dirty = false;
    pending = [];
    output = RHCodeHost(props);
    let nextProps = null;
    walk(output, (node) => {
      if (node.props?.ref) node.props.ref.current = root;
      if (node.type === retention.WorkRetention.Provider)
        acquire = node.props.value;
      if (node.type === View) nextProps = node.props;
    });
    if (nextProps) {
      if (!child) child = newController();
      nextProps.onController(child);
      childProps = nextProps;
    } else if (child) {
      childProps.onController(null);
      child = childProps = null;
    }
    for (const effect of pending) effect();
  }
  props = {
    api,
    native: options.native ?? true,
    visible: false,
    section: "code",
    onNavigate() {},
  };
  render();
  return {
    calls,
    imports,
    configurations,
    controllers,
    kernels,
    nativeResponses,
    prepareResponses,
    get child() {
      return child;
    },
    get output() {
      return output;
    },
    get now() {
      return now;
    },
    get activeTimers() {
      return timers.size;
    },
    setDialog(value) {
      dialog = value;
    },
    acquireWork: () => acquire(),
    visible(visible) {
      props = { ...props, visible };
      render();
    },
    async flush() {
      for (let index = 0; index < 30; index++) {
        await new Promise((done) => setImmediate(done));
        if (closed || !dirty) return;
        render();
      }
      throw new Error("Host did not settle");
    },
    tick(ms) {
      now += ms;
      for (const callback of [...timers.values()]) callback();
    },
    close() {
      if (closed) return;
      closed = true;
      for (const hook of hooks) hook?.cleanup?.();
      childProps?.onController(null);
    },
  };
}
async function fixture(t, options) {
  const driver = hostDriver(options);
  t.after(() => driver.close());
  await driver.flush();
  return driver;
}
async function enter(driver) {
  driver.visible(true);
  await driver.flush();
}
async function eligible(driver) {
  driver.visible(false);
  await driver.flush();
  driver.tick(10 * 60_000);
  await driver.flush();
}

test("initial launcher startup neither bootstraps nor imports the RH Code service", async (t) => {
  const driver = await fixture(t);
  assert.deepEqual(driver.calls, []);
  assert.deepEqual(driver.imports, []);
  assert.equal(driver.child, null);
  assert.equal(driver.output.props["data-code-state"], "dormant");
  assert.equal(driver.activeTimers, 0);
});

test("ordinary navigation leaves the exact RH Code/controller instance mounted during cooldown", async (t) => {
  const driver = await fixture(t);
  await enter(driver);
  const controller = driver.child;
  assert.ok(controller);
  assert.deepEqual(driver.imports, ["kernel", "plugins", "RHCode"]);
  driver.visible(false);
  await driver.flush();
  assert.equal(driver.child, controller);
  assert.equal(driver.output.props.hidden, true);
  driver.tick(10 * 60_000 - 1);
  await driver.flush();
  assert.equal(driver.child, controller);
  assert.equal(controller.prepares, 0);
  assert.equal(
    driver.calls.some((c) => c.command === "experimental_suspend"),
    false,
  );
});

test("idle departure saves dirty work and retires RH Code scopes only after preparation succeeds", async (t) => {
  const driver = await fixture(t);
  await enter(driver);
  const controller = driver.child;
  controller.state.dirty = true;
  await eligible(driver);
  assert.equal(controller.prepares, 1);
  assert.equal(controller.state.suspended, true);
  assert.equal(controller.state.dirty, false);
  assert.equal(driver.child, null);
  assert.equal(driver.output.props["data-code-state"], "dormant");
  assert.deepEqual(driver.configurations.at(-1).scopes, ["launcher"]);
});

test("a native host which was never started still permits saved UI retirement", async (t) => {
  const driver = await fixture(t);
  await enter(driver);
  const controller = driver.child;
  driver.nativeResponses.push(
    Promise.resolve({ released: false, busy: false }),
  );
  await eligible(driver);
  assert.equal(controller.prepares, 1);
  assert.equal(driver.child, null);
});

test("controller work and persisted AI jobs block native release even after a long silent interval", async (t) => {
  const driver = await fixture(t);
  await enter(driver);
  const controller = driver.child;
  controller.state.activeJobs = 1;
  await eligible(driver);
  assert.equal(controller.prepares, 0);
  assert.equal(
    driver.calls.some((c) => c.command === "experimental_suspend"),
    false,
  );
  controller.state.activeJobs = 0;
  controller.state.busy = 1;
  driver.tick(15_000);
  await driver.flush();
  assert.equal(controller.prepares, 0);
  assert.equal(driver.child, controller);
});

test("busy native work vetoes controller suspension", async (t) => {
  const driver = await fixture(t);
  await enter(driver);
  const controller = driver.child;
  driver.nativeResponses.push(Promise.resolve({ released: false, busy: true }));
  await eligible(driver);
  assert.equal(controller.prepares, 0);
  assert.equal(controller.state.suspended, false);
  assert.equal(driver.child, controller);
});

test("returning during delayed preparation cancels retirement and preserves the mounted controller", async (t) => {
  const driver = await fixture(t),
    preparation = deferred();
  await enter(driver);
  const controller = driver.child;
  driver.prepareResponses.push(preparation.promise);
  await eligible(driver);
  assert.equal(controller.prepares, 1);
  driver.visible(true);
  await driver.flush();
  preparation.resolve(true);
  await driver.flush();
  assert.equal(controller.state.suspended, false);
  assert.equal(driver.child, controller);
  assert.equal(driver.controllers.length, 1);
  assert.deepEqual(driver.configurations.at(-1).scopes, [
    "launcher",
    "rh-code",
  ]);
});

test("returning during a native suspend reply prevents controller preparation", async (t) => {
  const driver = await fixture(t),
    suspended = deferred();
  await enter(driver);
  const controller = driver.child;
  driver.nativeResponses.push(suspended.promise);
  await eligible(driver);
  driver.visible(true);
  await driver.flush();
  suspended.resolve({ released: true, busy: false });
  await driver.flush();
  assert.equal(controller.prepares, 0);
  assert.equal(driver.child, controller);
});

test("failed saving retains the mounted dirty controller for retry", async (t) => {
  const driver = await fixture(t);
  await enter(driver);
  const controller = driver.child;
  controller.state.dirty = true;
  driver.prepareResponses.push(Promise.resolve(false));
  await eligible(driver);
  assert.equal(driver.child, controller);
  assert.equal(controller.state.dirty, true);
  assert.deepEqual(driver.configurations.at(-1).scopes, [
    "launcher",
    "rh-code",
  ]);
});

test("reentry after full retirement mounts a fresh controller and restores saved data", async (t) => {
  const driver = await fixture(t);
  await enter(driver);
  const old = driver.child;
  await eligible(driver);
  await enter(driver);
  assert.notEqual(driver.child, old);
  assert.equal(driver.controllers.length, 2);
  assert.equal(driver.child.restored, "saved-project");
  assert.equal(driver.child.state.suspended, false);
  assert.equal(
    driver.kernels.length,
    1,
    "Plugin definitions survive scope retirement",
  );
  assert.deepEqual(driver.configurations.at(-1).scopes, [
    "launcher",
    "rh-code",
  ]);
});

test("view work leases and open dialogs each inhibit hidden UI retirement", async (t) => {
  const driver = await fixture(t);
  await enter(driver);
  const controller = driver.child,
    release = driver.acquireWork();
  await eligible(driver);
  assert.equal(controller.prepares, 0);
  release();
  driver.setDialog(true);
  driver.tick(15_000);
  await driver.flush();
  assert.equal(controller.prepares, 0);
  driver.setDialog(false);
  driver.tick(15_000);
  await driver.flush();
  assert.equal(driver.child, null);
});

test("launcher-scoped plugins retain their shared native host while Code retires", async (t) => {
  const driver = await fixture(t, {
    manifests: [...shipped, { id: "fixture.launcher", scope: "launcher" }],
  });
  await enter(driver);
  const controller = driver.child;
  await eligible(driver);
  assert.equal(controller.state.suspended, true);
  assert.equal(
    driver.calls.some((c) => c.command === "experimental_suspend"),
    false,
  );
  assert.equal(
    driver.kernels[0].statuses.get("fixture.launcher").state,
    "active",
  );
});

test("meaningful activity during a delayed native suspend restarts the controller idle cooldown", async (t) => {
  const driver = await fixture(t),
    native = deferred();
  await enter(driver);
  const controller = driver.child;
  driver.nativeResponses.push(native.promise);
  await eligible(driver);
  controller.state.lastActivityAt = driver.now;
  native.resolve({ released: true, busy: false });
  await driver.flush();
  assert.equal(
    controller.prepares,
    0,
    "The native await cannot consume a newly restarted idle interval",
  );
  assert.equal(driver.child, controller);
  driver.tick(5 * 60_000);
  await driver.flush();
  assert.equal(driver.child, null);
});

/** Small view driver verifies the real retention hook and target form, without
 * mounting unrelated editor views or invoking any project store. */
function viewDriver(file, name, initialCode) {
  const hooks = [],
    modules = new Map(),
    holders = new Set();
  let cursor = 0,
    pending = [],
    value,
    code = initialCode;
  const acquire = () => {
    const id = Symbol();
    holders.add(id);
    return () => holders.delete(id);
  };
  const react = {
    createContext: (initial) => ({ initial }),
    useContext: () => acquire,
    useState(initial) {
      const index = cursor++;
      if (!(index in hooks))
        hooks[index] = typeof initial === "function" ? initial() : initial;
      return [
        hooks[index],
        (next) => {
          hooks[index] = typeof next === "function" ? next(hooks[index]) : next;
        },
      ];
    },
    useRef: (initial) => (hooks[cursor++] ??= { current: initial }),
    useCallback(callback, deps) {
      const index = cursor++,
        previous = hooks[index];
      if (
        !previous ||
        deps.some((item, i) => !Object.is(item, previous.deps[i]))
      )
        hooks[index] = { deps, callback };
      return hooks[index].callback;
    },
    useEffect(effect, deps) {
      const index = cursor++,
        previous = hooks[index];
      if (
        !previous ||
        !deps ||
        deps.some((item, i) => !Object.is(item, previous.deps[i]))
      ) {
        hooks[index] = { deps };
        pending.push(() => {
          previous?.cleanup?.();
          hooks[index].cleanup = effect();
        });
      }
    },
  };
  const jsx = (type, props) => ({ type, props });
  function load(file) {
    if (modules.has(file)) return modules.get(file).exports;
    const module = { exports: {} };
    modules.set(file, module);
    const compiled = ts.transpileModule(readFileSync(file, "utf8"), {
      fileName: file,
      compilerOptions: {
        target: ts.ScriptTarget.ES2022,
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.ReactJSX,
      },
    }).outputText;
    const localRequire = (id) => {
      if (id === "react") return react;
      if (id === "react/jsx-runtime") return { jsx, jsxs: jsx };
      if (id === "lucide-react")
        return new Proxy({}, { get: () => () => null });
      if (id.endsWith("/controller")) return { useCode: () => code };
      if (id.endsWith("/locale")) return { label: (_, english) => english };
      if (id.startsWith("."))
        return load(path.resolve(path.dirname(file), id) + ".tsx");
      return require(id);
    };
    new Function("require", "module", "exports", compiled)(
      localRequire,
      module,
      module.exports,
    );
    return module.exports;
  }
  const component = load(path.join(source, file))[name];
  function render() {
    cursor = 0;
    pending = [];
    value = component({
      area: { type: "area", id: "fixture-targets", view: "targets" },
      api: { invoke: async () => {} },
    });
    for (const effect of pending) effect();
  }
  function nodes(node, result = []) {
    if (!node || typeof node !== "object") return result;
    if (Array.isArray(node)) {
      for (const child of node) nodes(child, result);
      return result;
    }
    result.push(node);
    nodes(node.props?.children, result);
    return result;
  }
  const text = (node) =>
    typeof node === "string"
      ? node
      : Array.isArray(node)
        ? node.map(text).join("")
        : text(node?.props?.children ?? "");
  render();
  return {
    holders,
    get value() {
      return value;
    },
    render,
    updateCode(next) {
      code = next;
      render();
    },
    input: () =>
      nodes(value).find(
        (node) =>
          node.type === "input" &&
          node.props["aria-label"]?.startsWith("Minecraft version"),
      ),
    save: () =>
      nodes(value).find(
        (node) => node.type === "button" && text(node).includes("Save"),
      ),
    close() {
      for (const hook of hooks) hook?.cleanup?.();
    },
  };
}

test("retention releases mounted leases and refuses new leases from late callbacks after unmount", () => {
  const view = viewDriver("rh-code/retention.tsx", "useMountRetention"),
    staleAcquire = view.value;
  const release = staleAcquire();
  assert.equal(view.holders.size, 1);
  view.close();
  assert.equal(view.holders.size, 0);
  staleAcquire();
  assert.equal(view.holders.size, 0);
  release();
  assert.equal(view.holders.size, 0);
});

test("target edits retain leases and values across unrelated snapshots and project switches", () => {
  const savedA = [{ minecraft: "1.20", loaders: ["Fabric"] }],
    savedB = [{ minecraft: "1.21", loaders: ["NeoForge"] }];
  const code = (id, targets) => ({
    projectId: id,
    project: { targets },
    native: true,
    busy: 0,
    write: async () => ({}),
  });
  const view = viewDriver(
    "rh-code/plugins/targets.tsx",
    "Targets",
    code("A", savedA),
  );
  view.input().props.onChange({ target: { value: "edited A" } });
  view.render();
  assert.equal(view.holders.size, 1);
  view.updateCode(code("A", structuredClone(savedA)));
  assert.equal(view.input().props.value, "edited A");
  view.updateCode(code("B", savedB));
  assert.equal(view.input().props.value, "1.21");
  assert.equal(
    view.holders.size,
    1,
    "An unselected project's form draft still pins its view",
  );
  view.input().props.onChange({ target: { value: "edited B" } });
  view.render();
  view.updateCode(code("A", structuredClone(savedA)));
  assert.equal(view.input().props.value, "edited A");
  view.updateCode(code("B", structuredClone(savedB)));
  assert.equal(view.input().props.value, "edited B");
  view.close();
  assert.equal(view.holders.size, 0);
});

test("late target save acknowledgement cannot erase a newer local edit", async () => {
  const saved = deferred(),
    writes = [];
  const code = {
    projectId: "A",
    project: { targets: [{ minecraft: "1.20", loaders: ["Fabric"] }] },
    native: true,
    busy: 0,
    write: async (_, args) => {
      writes.push(structuredClone(args));
      await saved.promise;
      return {};
    },
  };
  const view = viewDriver("rh-code/plugins/targets.tsx", "Targets", code);
  view.input().props.onChange({ target: { value: "submitted" } });
  view.render();
  view.save().props.onClick();
  view.input().props.onChange({ target: { value: "newer" } });
  view.render();
  saved.resolve();
  await new Promise((done) => setImmediate(done));
  view.render();
  assert.equal(writes[0].targets[0].minecraft, "submitted");
  assert.equal(view.input().props.value, "newer");
  assert.equal(view.holders.size, 1);
  view.close();
});
