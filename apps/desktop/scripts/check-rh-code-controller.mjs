/** Run the actual controller with isolated hook scheduling and delayed synthetic
 * host replies. No renderer, native project store, provider or real files run. */
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const source = fileURLToPath(new URL("../src/rh-code/", import.meta.url));
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

/** Each driver loads fresh source modules, including module-owned buffers and
 * selections. Only React scheduling and unused external host seams are mocked. */
function controllerDriver(api) {
  const hooks = [],
    modules = new Map();
  let cursor = 0,
    dirty = false,
    pending = [],
    value,
    closed = false;
  const react = {
    createContext: (initial) => ({ initial }),
    useContext: (context) => context.initial,
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
    useEffect(effect, deps) {
      const index = cursor++,
        previous = hooks[index];
      if (
        !previous ||
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
      if (id === "./locale") return { label: (_, english) => english };
      if (id === "../experimentalTypes")
        return {
          experimentalCall() {
            throw new Error("Unexpected agent invocation");
          },
        };
      if (id.startsWith("."))
        return load(path.resolve(path.dirname(file), id + ".ts"));
      return require(id);
    };
    new Function("require", "module", "exports", code)(
      localRequire,
      module,
      module.exports,
    );
    return module.exports;
  }
  const { useController } = load(path.join(source, "controller.tsx"));
  function render() {
    cursor = 0;
    dirty = false;
    pending = [];
    value = useController(api, true);
    for (const effect of pending) effect();
  }
  render();
  return {
    get value() {
      return value;
    },
    async flush() {
      for (let index = 0; index < 30; index++) {
        await new Promise((done) => setImmediate(done));
        if (closed || !dirty) return;
        render();
      }
      throw new Error("Controller did not settle");
    },
    close() {
      closed = true;
      for (const hook of hooks) hook?.cleanup?.();
    },
  };
}

const conversation = (id) => ({
  id,
  title: id,
  draft: "",
  mode: "plan",
  messages: [],
  activeJob: null,
});
const project = (id) => ({
  id,
  name: id,
  folder: "/synthetic",
  updatedAt: 0,
  targets: [{ minecraft: "fixture", loaders: ["fixture"] }],
  conversations: [conversation(id + "-thread")],
  layout: {
    type: "split",
    id: id + "-split",
    axis: "row",
    ratio: 0.5,
    children: [
      {
        type: "area",
        id: id + "-chat",
        view: "chat",
        conversation: id + "-thread",
      },
      {
        type: "area",
        id: id + "-source",
        view: "source",
        file: "src/Main.java",
      },
    ],
  },
});
function hostFixture() {
  const data = {
    revision: "revision-0",
    projects: [project("A"), project("B")],
    disabledPlugins: [],
    plugins: [],
    layoutPresets: [],
  };
  const calls = [],
    handlers = new Map();
  let revision = 0;
  return {
    data,
    calls,
    handlers,
    snapshot: () => structuredClone(data),
    async api(command, { operation, args }) {
      assert.equal(command, "rh_code_call");
      calls.push({ operation, args: structuredClone(args) });
      if (operation === "bootstrap") return structuredClone(data);
      if (handlers.has(operation)) return handlers.get(operation)(args);
      assert.equal(args.expectedRevision, data.revision);
      const owner = data.projects.find(
        (project) => project.id === args.projectId,
      );
      if (operation === "layout_save")
        owner.layout = structuredClone(args.layout);
      else if (operation === "conversation_create")
        owner.conversations.push(conversation("created-thread"));
      else throw new Error(`Unexpected operation: ${operation}`);
      data.revision = `revision-${++revision}`;
      return structuredClone(data);
    },
  };
}
async function fixture(t) {
  const host = hostFixture(),
    ui = controllerDriver(host.api);
  t.after(() => ui.close());
  await ui.flush();
  ui.value.choose(ui.value.snapshot.projects[0]);
  await ui.flush();
  return { host, ui };
}

test("pending layout geometry and per-view state survive A -> B -> A before save acknowledgement", async (t) => {
  const { host, ui } = await fixture(t),
    save = deferred();
  host.handlers.set("layout_save", async (args) => {
    await save.promise;
    assert.equal(args.projectId, "A");
    host.data.projects[0].layout = structuredClone(args.layout);
    host.data.revision = "revision-1";
    return host.snapshot();
  });
  const desired = structuredClone(ui.value.layout);
  desired.ratio = 0.7;
  desired.children[0].state = { chat: { folded: true } };
  ui.value.mutateLayout(desired);
  await ui.flush();
  ui.value.choose(ui.value.snapshot.projects[1]);
  await ui.flush();
  assert.equal(ui.value.projectId, "B");
  ui.value.choose(ui.value.snapshot.projects[0]);
  await ui.flush();
  assert.deepEqual(
    ui.value.layout,
    desired,
    "Switching back must use the pending project-owned layout",
  );
  save.resolve();
  await ui.flush();
  assert.deepEqual(ui.value.layout, desired);
  assert.deepEqual(ui.value.snapshot.projects[0].layout, desired);
  assert.equal(ui.value.busy, 0);
});

test("late conversation creation after switching projects does not copy B layout into A", async (t) => {
  const { host, ui } = await fixture(t),
    created = deferred();
  const originalA = structuredClone(host.data.projects[0].layout),
    originalB = structuredClone(host.data.projects[1].layout);
  host.handlers.set("conversation_create", async (args) => {
    await created.promise;
    assert.equal(args.projectId, "A");
    host.data.projects[0].conversations.push(conversation("created-thread"));
    host.data.revision = "revision-1";
    return host.snapshot();
  });
  const request = ui.value.newConversation("A-chat");
  await ui.flush();
  ui.value.choose(ui.value.snapshot.projects[1]);
  await ui.flush();
  created.resolve();
  assert.equal(await request, "created-thread");
  await ui.flush();
  assert.equal(ui.value.projectId, "B");
  assert.deepEqual(ui.value.layout, originalB);
  assert.deepEqual(ui.value.snapshot.projects[0].layout, originalA);
  assert.equal(
    host.calls.filter((call) => call.operation === "layout_save").length,
    0,
  );
});

test("a second delayed source read retains edits made after the first source area opened", async (t) => {
  const { host, ui } = await fixture(t),
    first = deferred(),
    second = deferred();
  let reads = 0;
  host.handlers.set("file_read", (args) => {
    assert.deepEqual(args, { projectId: "A", path: "src/Main.java" });
    return (++reads === 1 ? first : second).promise;
  });
  const one = ui.value.loadFile("src/Main.java"),
    two = ui.value.loadFile("src/Main.java");
  assert.equal(reads, 2);
  first.resolve({ content: "disk baseline", sha256: "first-hash" });
  const opened = await one;
  await ui.flush();
  ui.value.edit("src/Main.java", "unsaved edit from first area");
  await ui.flush();
  second.resolve({ content: "late disk baseline", sha256: "second-hash" });
  assert.equal(
    await two,
    opened,
    "Both areas must share the already-open buffer",
  );
  await ui.flush();
  assert.deepEqual(ui.value.buffer("src/Main.java"), {
    content: "unsaved edit from first area",
    original: "disk baseline",
    sha256: "first-hash",
  });
});
