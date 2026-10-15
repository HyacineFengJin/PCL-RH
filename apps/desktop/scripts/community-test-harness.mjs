/** Actual community component hooks with child dialogs left at their public
 * props. Each test owns an in-memory API; no native browser or game is used. */
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
const require = createRequire(import.meta.url);
const ts = require("typescript");
const source = fileURLToPath(new URL("../src/", import.meta.url));
export { content, elements, deferred, button } from "./maker-test-harness.mjs";

export function communityHarness(name, initial = {}) {
  const hooks = [],
    modules = new Map();
  let props = initial,
    tree,
    cursor = 0,
    dirty = false,
    effects = [],
    closed = false;
  const react = {
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
    useContext(context) {
      return context.value;
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
        !old ||
        !deps ||
        deps.some((value, j) => !Object.is(value, old.deps[j]))
      ) {
        hooks[i] = { deps };
        effects.push(() => {
          old?.cleanup?.();
          hooks[i].cleanup = effect();
        });
      }
    },
  };
  function placeholder(name) {
    return { [name]: { [name]: function () {} }[name] };
  }
  const stubs = {
    "./CeSelect": placeholder("CeSelect"),
    "./Collapse": placeholder("Collapse"),
    "./LoaderCatalog": {
      Favorites() {},
      LoaderCatalog() {},
      installerPages: [],
    },
    "./InstallSelection": { InstallSelection() {}, installNameError() {} },
    "./ResourceInstall": placeholder("ResourceInstall"),
    "./ResourceSave": placeholder("ResourceSave"),
    "./InstanceImport": placeholder("InstanceImport"),
    "./LauncherFavorites": placeholder("ResourceFavorite"),
    "./useLauncherPreferences": {
      LauncherNavigationContext: { value: { standaloneSaveAvailable: true } },
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
      if (stubs[id]) return stubs[id];
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
    new Function("require", "module", "exports", "window", code)(
      localRequire,
      module,
      module.exports,
      {
        requestAnimationFrame(fn) {
          fn();
        },
      },
    );
    return module.exports;
  }
  const file = name === "CommunityCatalog" ? "DownloadPanel" : name;
  const component = load(path.join(source, `${file}.tsx`))[name];
  const i18n = load(path.join(source, "i18n.ts"));
  function render() {
    cursor = 0;
    dirty = false;
    effects = [];
    tree = component(props);
    for (const effect of effects) effect();
  }
  render();
  return {
    i18n,
    get tree() {
      return tree;
    },
    get props() {
      return props;
    },
    async flush() {
      for (let n = 0; n < 35; n++) {
        await new Promise((done) => setImmediate(done));
        if (!dirty || closed) return;
        render();
      }
      throw Error("Component did not settle");
    },
    replace(next) {
      props = { ...props, ...next };
      render();
    },
    close() {
      closed = true;
      for (const hook of hooks) hook?.cleanup?.();
    },
    module(name) {
      return load(path.join(source, `${name}.ts`));
    },
  };
}
