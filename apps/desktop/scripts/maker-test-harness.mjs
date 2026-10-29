/** Real component event handlers with isolated hook scheduling. Child panels
 * remain separate components and are tested with their own ownership scopes. */
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
const require = createRequire(import.meta.url),
  ts = require("typescript");
const source = fileURLToPath(new URL("../src/", import.meta.url));
export function harness(name, props = {}, options = {}) {
  const modules = new Map(),
    hooks = [],
    timers = new Map(),
    listeners = new Map();
  let cursor = 0,
    dirty = false,
    pending = [],
    tree,
    closed = false;
  const react = {
    Component: require("react").Component,
    // The harness leaves child providers unmounted. Parent modules may still
    // create contexts at import time; reads use their declared default value.
    createContext(value) {
      return { value };
    },
    useContext(context) {
      return context.value;
    },
    useState(initial) {
      const i = cursor++;
      if (!(i in hooks))
        hooks[i] = typeof initial === "function" ? initial() : initial;
      return [
        hooks[i],
        (next) => {
          const value = typeof next === "function" ? next(hooks[i]) : next;
          if (!Object.is(value, hooks[i])) {
            hooks[i] = value;
            dirty = true;
          }
        },
      ];
    },
    useRef(value) {
      return (hooks[cursor++] ??= { current: value });
    },
    useMemo(factory, deps) {
      const i = cursor++,
        old = hooks[i];
      if (!old || deps.some((value, j) => !Object.is(value, old.deps[j])))
        hooks[i] = { deps, value: factory() };
      return hooks[i].value;
    },
    useEffect(effect, deps) {
      const i = cursor++,
        old = hooks[i];
      if (
        !deps ||
        !old ||
        deps.some((value, j) => !Object.is(value, old.deps[j]))
      ) {
        hooks[i] = { deps };
        pending.push(() => {
          old?.cleanup?.();
          hooks[i].cleanup = effect();
        });
      }
    },
    useLayoutEffect(effect, deps) {
      react.useEffect(effect, deps);
    },
    useSyncExternalStore(subscribe, getSnapshot) {
      const [, update] = react.useState(getSnapshot);
      react.useEffect(() => {
        const check = () => update(() => getSnapshot());
        const release = subscribe(check);
        check();
        return release;
      }, [subscribe, getSnapshot]);
      return getSnapshot();
    },
  };
  const browser = {
    innerWidth: 1440,
    addEventListener(type, listener) {
      if (!listeners.has(type)) listeners.set(type, new Set());
      listeners.get(type).add(listener);
    },
    removeEventListener(type, listener) {
      listeners.get(type)?.delete(listener);
    },
    setInterval(fn) {
      const id = {};
      timers.set(id, fn);
      return id;
    },
    setTimeout(fn) {
      const id = {};
      timers.set(id, () => {
        timers.delete(id);
        fn();
      });
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
    clearInterval(id) {
      timers.delete(id);
    },
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
      if (id === "./CeSelect") return { CeSelect: function CeSelect() {} };
      if (id === "./instanceOperationUi")
        return {
          InstanceOperationDialog: function InstanceOperationDialog() {},
        };
      if (id.endsWith(".css")) return {};
      if (id.endsWith(".png")) return id;
      if (id.startsWith(".")) {
        const base = path.resolve(path.dirname(file), id);
        return load(
          fs.existsSync(base + ".tsx") ? base + ".tsx" : base + ".ts",
        );
      }
      return require(id);
    }
    new Function(
      "require",
      "module",
      "exports",
      "window",
      "setTimeout",
      "clearTimeout",
      "document",
      code,
    )(
      localRequire,
      module,
      module.exports,
      browser,
      browser.setTimeout,
      browser.clearTimeout,
      { activeElement: null, documentElement: { lang: "zh-CN" } },
    );
    return module.exports;
  }
  const component = load(
      path.join(
        source,
        options.module ||
          `${name === "MakerRoundView" ? "MakerThreadView" : name === "MakerCharter" ? "MakerProjectPanels" : name}.tsx`,
      ),
    ),
    i18n = load(path.join(source, "i18n.ts"));
  const state = load(path.join(source, "makerWorkspaceState.ts"));
  if (name === "ExperimentalProjects" && !props.draft)
    props.draft = { current: state.createProjectManagerDraft() };
  function render() {
    cursor = 0;
    dirty = false;
    pending = [];
    tree = options.render
      ? options.render(component, props)
      : component[name](props);
    for (const effect of pending) effect();
  }
  async function flush() {
    for (let n = 0; n < 30; n++) {
      await new Promise((done) => setImmediate(done));
      if (!dirty || closed) return tree;
      render();
    }
    throw new Error("Component did not settle");
  }
  render();
  return {
    props,
    i18n,
    state,
    flush,
    get tree() {
      return tree;
    },
    replace(next) {
      props = { ...props, ...next };
      render();
    },
    async poll() {
      for (const fn of timers.values()) fn();
      await flush();
    },
    async dispatch(type) {
      for (const listener of [...(listeners.get(type) || [])]) listener();
      await flush();
    },
    listenerCount: (type) => listeners.get(type)?.size || 0,
    timerCount: () => timers.size,
    close() {
      closed = true;
      for (const hook of hooks) hook?.cleanup?.();
    },
  };
}
export function elements(tree, match) {
  if (!tree || typeof tree !== "object") return [];
  if (Array.isArray(tree)) return tree.flatMap((node) => elements(node, match));
  return [
    ...(match(tree) ? [tree] : []),
    ...elements(tree.props?.children, match),
  ];
}
export function content(tree) {
  if (Array.isArray(tree)) return tree.map(content).join("");
  if (tree && typeof tree === "object") return content(tree.props?.children);
  return typeof tree === "string" || typeof tree === "number"
    ? String(tree)
    : "";
}
export function button(ui, key) {
  const text = ui.i18n.t(key);
  const value = elements(
    ui.tree,
    (node) => node.type === "button" && content(node) === text,
  )[0];
  assert.ok(value, `Missing button ${key}`);
  return value;
}
export function field(ui, key) {
  const text = ui.i18n.t(key);
  const label = elements(
    ui.tree,
    (node) =>
      node.type === "label" &&
      elements(
        node,
        (child) => child.type === "span" && child.props.children === text,
      ).length,
  )[0];
  assert.ok(label, `Missing label ${key}`);
  return elements(
    label,
    (node) =>
      ["input", "textarea"].includes(node.type) ||
      node.type?.name === "CeSelect",
  )[0];
}
export async function click(ui, key) {
  const value = button(ui, key);
  assert.ok(!value.props.disabled, `Disabled ${key}`);
  value.props.onClick();
  await ui.flush();
}
export async function change(ui, key, value) {
  field(ui, key).props.onChange({ target: { value } });
  await ui.flush();
}
export function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
export const checkpoint = {
  id: "point-1",
  label: "Initial",
  jobId: "job-1",
  operationId: "generate-1",
  fingerprint: "a".repeat(64),
  workflow: "maker",
  createdAt: "2026-01-01T00:00:00.000Z",
};
export const project = (id = "project-1") => ({
  id,
  name: id,
  notes: "Saved requirements",
  revision: "revision-1",
  archived: false,
  updatedAt: checkpoint.createdAt,
  checkpoints: [checkpoint],
});
export const engine = {
  jobs: [],
  aiPresets: {
    selectedId: "preset-1",
    presets: [
      {
        id: "preset-1",
        name: "Saved preset",
        selection: { model: "offline-fixture" },
      },
    ],
  },
};
