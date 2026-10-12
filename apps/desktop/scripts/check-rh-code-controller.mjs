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
  let hooks = [];
  const modules = new Map();
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
      const ownedHooks = hooks;
      return [
        hooks[index],
        (next) => {
          const result =
            typeof next === "function" ? next(ownedHooks[index]) : next;
          if (!Object.is(result, ownedHooks[index])) {
            ownedHooks[index] = result;
            if (ownedHooks === hooks) dirty = true;
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
          experimentalCall(api, operation, args = {}) {
            return api("experimental_call", { operation, args });
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
    remount() {
      assert.equal(closed, true);
      hooks = [];
      closed = false;
      render();
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
  contextNote: "",
  contextPaths: [],
});
const project = (id) => ({
  id,
  name: id,
  folder: "/synthetic",
  updatedAt: 0,
  targets: [{ minecraft: "fixture", loaders: ["fixture"] }],
  conversations: [conversation(id + "-thread")],
  instructions: "",
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
    handlers = new Map(),
    files = new Map(
      ["A", "B"].map((id) => [
        id + ":src/Main.java",
        { content: id + " source", sha256: id + "-hash" },
      ]),
    );
  let revision = 0;
  return {
    data,
    calls,
    handlers,
    files,
    commit() {
      data.revision = `revision-${++revision}`;
      return structuredClone(data);
    },
    snapshot: () => structuredClone(data),
    async api(command, { operation, args }) {
      operation ??= command;
      assert.ok(
        [
          "rh_code_call",
          "experimental_call",
          "rh_code_adopt",
          "rh_code_harness_apply",
          "rh_code_harness_reviews",
        ].includes(command),
      );
      calls.push({ operation, args: structuredClone(args) });
      if (operation === "bootstrap") return structuredClone(data);
      if (operation === "status")
        return {
          jobs: [],
          liveConfigured: false,
          liveAvailable: true,
          aiPresets: { selectedId: "preset", presets: [] },
        };
      if (handlers.has(operation)) return handlers.get(operation)(args);
      if (operation === "file_read")
        return structuredClone(files.get(args.projectId + ":" + args.path));
      assert.equal(args.expectedRevision, data.revision);
      const owner = data.projects.find(
        (project) => project.id === args.projectId,
      );
      if (operation === "layout_save")
        owner.layout = structuredClone(args.layout);
      else if (operation === "conversation_create")
        owner.conversations.push(conversation("created-thread"));
      else if (operation === "project_instructions")
        owner.instructions = args.instructions;
      else if (
        operation === "conversation_context" ||
        operation === "conversation_rename"
      ) {
        const target = owner.conversations.find(
          (c) => c.id === args.conversationId,
        );
        if (operation === "conversation_rename") target.title = args.title;
        else {
          target.contextNote = args.contextNote;
          target.contextPaths = structuredClone(args.contextPaths);
        }
      } else if (operation === "conversation_draft") {
        const target = owner.conversations.find(
          (c) => c.id === args.conversationId,
        );
        target.draft = args.draft;
        if (args.mode) target.mode = args.mode;
      } else if (
        operation === "conversation_append" ||
        operation === "conversation_complete"
      ) {
        const target = owner.conversations.find(
          (c) => c.id === args.conversationId,
        );
        target.messages.push({
          id: "message",
          role: args.role,
          content: args.content,
          jobId: args.jobId,
        });
        target.activeJob = args.activeJob;
      } else if (operation === "file_save") {
        const file = files.get(args.projectId + ":" + args.path);
        assert.equal(
          args.sha256,
          file.sha256,
          "Source changed on disk; refresh and compare before saving",
        );
        file.content = args.content;
        file.sha256 = "saved-" + ++revision;
      } else throw new Error(`Unexpected operation: ${operation}`);
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

async function openSource(ui, content) {
  await ui.value.loadFile("src/Main.java");
  await ui.flush();
  ui.value.edit("src/Main.java", content);
  await ui.flush();
}

test("Harness capture waits for dirty source saves and refuses an external conflict before admission", async (t) => {
  const { host, ui } = await fixture(t);
  await openSource(ui, "saved input");
  host.handlers.set("rh_code_run", async () => {
    assert.equal(host.files.get("A:src/Main.java").content, "saved input");
    throw new Error("end isolated preparation");
  });
  await ui.value.run("A-chat", "A-thread", "ask", "inspect");
  assert.ok(
    host.calls.findIndex((c) => c.operation === "file_save") <
      host.calls.findIndex((c) => c.operation === "rh_code_run"),
  );
  ui.value.edit("src/Main.java", "new draft");
  host.files.set("A:src/Main.java", {
    content: "external",
    sha256: "external-hash",
  });
  const before = host.calls.filter((c) => c.operation === "rh_code_run").length;
  await ui.value.run("A-chat", "A-thread", "execute", "modify");
  assert.equal(
    host.calls.filter((c) => c.operation === "rh_code_run").length,
    before,
  );
  assert.equal(ui.value.buffer("src/Main.java").content, "new draft");
});

test("Harness application shares metadata ordering and retains edits made during native publication", async (t) => {
  const { host, ui } = await fixture(t),
    applying = deferred();
  await ui.value.loadFile("src/Main.java");
  await ui.flush();
  let admittedRevision;
  host.handlers.set("rh_code_harness_apply", async (args) => {
    admittedRevision = args.expectedRevision;
    await applying.promise;
    assert.equal(args.projectId, "A");
    host.files.set("A:src/Main.java", {
      content: "published",
      sha256: "published-hash",
    });
    return host.commit();
  });
  const apply = ui.value.applyHarnessProposal("job", "proposal", "digest");
  await ui.flush();
  assert.equal(admittedRevision, "revision-0");
  const metadata = ui.value.write("conversation_draft", {
    projectId: "A",
    conversationId: "A-thread",
    draft: "new metadata",
  });
  await ui.flush();
  assert.equal(
    host.calls.filter((c) => c.operation === "conversation_draft").length,
    0,
  );
  ui.value.edit("src/Main.java", "typed while publishing");
  await ui.flush();
  ui.value.choose(ui.value.snapshot.projects[1]);
  await ui.flush();
  applying.resolve();
  await apply;
  await metadata;
  await ui.flush();
  assert.equal(ui.value.projectId, "B");
  assert.equal(ui.value.snapshot.revision, host.data.revision);
  ui.value.choose(ui.value.snapshot.projects[0]);
  await ui.flush();
  assert.equal(
    ui.value.buffer("src/Main.java").content,
    "typed while publishing",
  );
  assert.equal(ui.value.buffer("src/Main.java").sha256, "A-hash");
});

test("native commit followed by host receipt failure refreshes revision for an idempotent retry", async (t) => {
  const { host, ui } = await fixture(t);
  await ui.value.loadFile("src/Main.java");
  await ui.flush();
  let attempts = 0;
  host.handlers.set("rh_code_harness_apply", async (args) => {
    assert.equal(args.expectedRevision, host.data.revision);
    if (++attempts === 1) {
      host.files.set("A:src/Main.java", {
        content: "committed",
        sha256: "committed-hash",
      });
      host.commit();
      throw new Error("host receipt unavailable");
    }
    return host.snapshot();
  });
  await assert.rejects(
    ui.value.applyHarnessProposal("job", "proposal", "digest"),
    /host receipt/,
  );
  await ui.flush();
  assert.equal(ui.value.snapshot.revision, host.data.revision);
  assert.equal(ui.value.buffer("src/Main.java"), undefined);
  await ui.value.applyHarnessProposal("job", "proposal", "digest");
  assert.equal(attempts, 2);
  await ui.value.loadFile("src/Main.java");
  await ui.flush();
  assert.equal(ui.value.buffer("src/Main.java").content, "committed");
});

test("source edits arriving during a pre-Harness save remain dirty and do not start the model", async (t) => {
  const { host, ui } = await fixture(t),
    saved = deferred();
  await openSource(ui, "submitted source");
  host.handlers.set("file_save", async (args) => {
    await saved.promise;
    host.files.set("A:src/Main.java", {
      content: args.content,
      sha256: "new-hash",
    });
    return host.commit();
  });
  const run = ui.value.run("A-chat", "A-thread", "execute", "change source");
  await ui.flush();
  ui.value.edit("src/Main.java", "newer unsaved source");
  saved.resolve();
  await run;
  await ui.flush();
  assert.equal(
    host.calls.some((c) => c.operation === "rh_code_run"),
    false,
  );
  assert.equal(
    ui.value.buffer("src/Main.java").content,
    "newer unsaved source",
  );
  assert.equal(ui.value.getLifecycleState().dirty, true);
});

test("suspension flushes drafts and source edits owned by every project, then releases caches", async (t) => {
  const { host, ui } = await fixture(t);
  const prepare = ui.value.prepareSuspend,
    state = ui.value.getLifecycleState;
  await openSource(ui, "saved A");
  ui.value.setDraft("A-thread", "draft A");
  ui.value.choose(ui.value.snapshot.projects[1]);
  await ui.flush();
  await openSource(ui, "saved B");
  ui.value.setDraft("B-thread", "draft B");
  await ui.flush();
  assert.equal(
    ui.value.prepareSuspend,
    prepare,
    "Lifecycle methods retain identity across renders",
  );
  assert.equal(ui.value.getLifecycleState, state);
  assert.equal(state().dirty, true);
  assert.equal(await prepare(), true);
  assert.equal(state().suspended, true);
  assert.equal(state().dirty, false);
  assert.equal(state().busy, 0);
  assert.equal(host.files.get("A:src/Main.java").content, "saved A");
  assert.equal(host.files.get("B:src/Main.java").content, "saved B");
  assert.equal(host.data.projects[0].conversations[0].draft, "draft A");
  assert.equal(host.data.projects[1].conversations[0].draft, "draft B");
  await ui.flush();
  assert.equal(ui.value.snapshot, null);
  assert.equal(ui.value.buffer("src/Main.java"), undefined);
  assert.equal(ui.value.draft("B-thread"), "");
  assert.deepEqual(
    host.calls
      .filter((c) => c.operation === "file_save")
      .map((c) => c.args.sha256),
    ["A-hash", "B-hash"],
  );
  await assert.rejects(
    ui.value.api("rh_code_call", { operation: "files", args: {} }),
    /suspended/,
  );
});

test("source reads pin immediately and late replies after unmount cannot enter the next service", async (t) => {
  const { host, ui } = await fixture(t),
    read = deferred();
  host.handlers.set("file_read", () => read.promise);
  const previous = ui.value,
    request = previous.loadFile("src/Main.java");
  assert.equal(previous.getLifecycleState().busy, 1);
  assert.equal(await previous.prepareSuspend(), false);
  ui.close();
  ui.remount();
  await ui.flush();
  read.resolve({ content: "old late content", sha256: "late-hash" });
  assert.equal(await request, undefined);
  await ui.flush();
  assert.equal(
    ui.value.projectId,
    "A",
    "Small selection locator survives remount",
  );
  assert.equal(ui.value.buffer("src/Main.java"), undefined);
  assert.equal(previous.getLifecycleState().busy, 0);
  host.handlers.delete("file_read");
  await ui.value.loadFile("src/Main.java");
  assert.equal(ui.value.buffer("src/Main.java").content, "A source");
});

test("source edits typed during autosave remain dirty and refuse suspension", async (t) => {
  const { host, ui } = await fixture(t),
    save = deferred();
  await openSource(ui, "submitted content");
  host.handlers.set("file_save", async (args) => {
    await save.promise;
    assert.equal(args.sha256, "A-hash");
    host.files.set("A:src/Main.java", {
      content: args.content,
      sha256: "new-hash",
    });
    return host.commit();
  });
  const preparation = ui.value.prepareSuspend();
  await ui.flush();
  ui.value.edit("src/Main.java", "newer content");
  await ui.flush();
  save.resolve();
  assert.equal(await preparation, false);
  await ui.flush();
  assert.deepEqual(ui.value.buffer("src/Main.java"), {
    content: "newer content",
    original: "submitted content",
    sha256: "new-hash",
  });
  assert.equal(ui.value.getLifecycleState().dirty, true);
  assert.equal(ui.value.getLifecycleState().suspended, false);
  host.handlers.delete("file_save");
  assert.equal(await ui.value.prepareSuspend(), true);
  assert.equal(host.files.get("A:src/Main.java").content, "newer content");
});

test("the return-to-UI predicate cancels suspension after delayed native save", async (t) => {
  const { host, ui } = await fixture(t),
    saved = deferred();
  await openSource(ui, "save while hidden");
  host.handlers.set("file_save", async (args) => {
    await saved.promise;
    host.files.set("A:src/Main.java", {
      content: args.content,
      sha256: "saved",
    });
    return host.commit();
  });
  let hidden = true;
  const preparing = ui.value.prepareSuspend(() => hidden);
  await ui.flush();
  hidden = false;
  saved.resolve();
  assert.equal(await preparing, false);
  assert.equal(ui.value.getLifecycleState().suspended, false);
  assert.equal(ui.value.buffer("src/Main.java").content, "save while hidden");
  assert.equal(await ui.value.prepareSuspend(() => false), false);
  assert.equal(await ui.value.prepareSuspend(() => true), true);
});

test("source conflict retains content and its original SHA guard", async (t) => {
  const { host, ui } = await fixture(t);
  await openSource(ui, "user edit");
  host.files.set("A:src/Main.java", {
    content: "external edit",
    sha256: "external-hash",
  });
  assert.equal(await ui.value.prepareSuspend(), false);
  await ui.flush();
  assert.equal(ui.value.buffer("src/Main.java").content, "user edit");
  assert.equal(ui.value.buffer("src/Main.java").sha256, "A-hash");
  assert.equal(ui.value.getLifecycleState().dirty, true);
  assert.equal(ui.value.getLifecycleState().suspended, false);
  assert.equal(host.files.get("A:src/Main.java").content, "external edit");
  assert.equal(await ui.value.prepareSuspend(), false);
});

test("failed debounced drafts retain their text and can be retried by suspension", async (t) => {
  const { host, ui } = await fixture(t);
  host.handlers.set("conversation_draft", async () => {
    throw new Error("draft save failed");
  });
  ui.value.setDraft("A-thread", "retain failed text");
  assert.equal(await ui.value.prepareSuspend(), false);
  await ui.flush();
  ui.value.choose(ui.value.snapshot.projects[1]);
  await ui.flush();
  ui.value.choose(ui.value.snapshot.projects[0]);
  await ui.flush();
  assert.equal(ui.value.draft("A-thread"), "retain failed text");
  assert.equal(ui.value.getLifecycleState().dirty, true);
  host.handlers.delete("conversation_draft");
  assert.equal(await ui.value.prepareSuspend(), true);
  assert.equal(
    host.data.projects[0].conversations[0].draft,
    "retain failed text",
  );
});

test("failed layout persistence retains geometry and suspension retries it", async (t) => {
  const { host, ui } = await fixture(t);
  host.handlers.set("layout_save", async () => {
    throw new Error("layout save failed");
  });
  const layout = structuredClone(ui.value.layout);
  layout.ratio = 0.8;
  ui.value.mutateLayout(layout);
  await ui.flush();
  assert.deepEqual(ui.value.layout, layout);
  assert.equal(ui.value.getLifecycleState().dirty, true);
  assert.equal(await ui.value.prepareSuspend(), false);
  host.handlers.delete("layout_save");
  assert.equal(await ui.value.prepareSuspend(), true);
  assert.deepEqual(host.data.projects[0].layout, layout);
});

test("queued mutation is counted before first await and suspension drains its acknowledgement", async (t) => {
  const { host, ui } = await fixture(t),
    written = deferred();
  host.handlers.set("layout_save", async (args) => {
    await written.promise;
    host.data.projects[0].layout = structuredClone(args.layout);
    return host.commit();
  });
  const request = ui.value.write("layout_save", {
    projectId: "A",
    layout: ui.value.layout,
  });
  assert.equal(ui.value.getLifecycleState().busy, 1);
  const preparing = ui.value.prepareSuspend();
  await ui.flush();
  assert.equal(ui.value.getLifecycleState().suspended, false);
  written.resolve();
  await request;
  assert.equal(await preparing, true);
});

test("unknown view requests pin the service before first await", async (t) => {
  const { host, ui } = await fixture(t),
    read = deferred();
  host.handlers.set("files", () => read.promise);
  const request = ui.value.api("rh_code_call", {
    operation: "files",
    args: { projectId: "A" },
  });
  assert.equal(ui.value.getLifecycleState().busy, 1);
  assert.equal(await ui.value.prepareSuspend(), false);
  read.resolve({ files: [] });
  await request;
  assert.equal(await ui.value.prepareSuspend(), true);
});

test("queued AI admission and persisted active jobs refuse suspension", async (t) => {
  const { host, ui } = await fixture(t),
    created = deferred();
  host.handlers.set("rh_code_run", () => created.promise);
  host.handlers.set("rh_code_start", async () => ({}));
  host.handlers.set("job_read", async () => ({
    summary: { jobId: "job", revision: 1, status: "queued" },
    result: null,
  }));
  const request = ui.value.run("A-chat", "A-thread", "plan", "start work");
  assert.ok(
    ui.value.getLifecycleState().activeJobs > 0,
    "AI reservation is synchronous",
  );
  assert.equal(await ui.value.prepareSuspend(), false);
  await ui.flush();
  created.resolve({ jobId: "job" });
  await request;
  await ui.flush();
  assert.equal(ui.value.snapshot.projects[0].conversations[0].activeJob, "job");
  assert.ok(ui.value.getLifecycleState().activeJobs > 0);
  assert.equal(await ui.value.prepareSuspend(), false);
});

test("completion persistence pins suspension until assistant output is saved", async (t) => {
  const { host, ui } = await fixture(t),
    completed = deferred();
  host.handlers.set("job_read", async () => ({
    summary: { jobId: "job", revision: 2, status: "completed" },
    result: { text: "assistant result" },
  }));
  host.handlers.set("conversation_complete", async (args) => {
    await completed.promise;
    const conversation = host.data.projects[0].conversations[0];
    conversation.messages.push({ role: args.role, content: args.content });
    conversation.activeJob = null;
    return host.commit();
  });
  host.data.projects[0].conversations[0].activeJob = "job";
  await ui.value.read();
  await ui.flush();
  assert.equal(await ui.value.prepareSuspend(), false);
  completed.resolve();
  await ui.flush();
  assert.equal(ui.value.getLifecycleState().activeJobs, 0);
  assert.equal(
    host.data.projects[0].conversations[0].messages[0].content,
    "assistant result",
  );
  assert.equal(await ui.value.prepareSuspend(), true);
});

test("old launch completion cannot clear a newer draft in another selected project", async (t) => {
  const { host, ui } = await fixture(t),
    start = deferred();
  host.handlers.set("rh_code_run", async () => ({ jobId: "job" }));
  host.handlers.set("rh_code_start", () => start.promise);
  host.handlers.set("job_read", async () => ({
    summary: { jobId: "job", revision: 1, status: "running" },
    result: null,
  }));
  ui.value.setDraft("A-thread", "submitted draft");
  const run = ui.value.run("A-chat", "A-thread", "plan");
  await ui.flush();
  ui.value.setDraft("A-thread", "newer A draft");
  ui.value.choose(ui.value.snapshot.projects[1]);
  await ui.flush();
  ui.value.setDraft("B-thread", "B draft");
  start.resolve({});
  await run;
  await ui.flush();
  assert.equal(ui.value.draft("B-thread"), "B draft");
  ui.value.choose(ui.value.snapshot.projects[0]);
  await ui.flush();
  assert.equal(ui.value.draft("A-thread"), "newer A draft");
});

test("accepted multi-project drafts finish their revisioned queue after unmount without restoring caches", async (t) => {
  const { host, ui } = await fixture(t);
  ui.value.setDraft("A-thread", "A before unmount");
  ui.value.choose(ui.value.snapshot.projects[1]);
  await ui.flush();
  ui.value.setDraft("B-thread", "B before unmount");
  const old = ui.value;
  ui.close();
  await new Promise((done) => setImmediate(done));
  assert.equal(
    host.data.projects[0].conversations[0].draft,
    "A before unmount",
  );
  assert.equal(
    host.data.projects[1].conversations[0].draft,
    "B before unmount",
  );
  assert.equal(old.draft("B-thread"), "");
  assert.equal(old.getLifecycleState().busy, 0);
});

test("a newer draft typed during draft persistence survives its older acknowledgement", async (t) => {
  const { host, ui } = await fixture(t),
    written = deferred();
  host.handlers.set("conversation_draft", async (args) => {
    await written.promise;
    host.data.projects[0].conversations[0].draft = args.draft;
    return host.commit();
  });
  ui.value.setDraft("A-thread", "older draft");
  const preparing = ui.value.prepareSuspend();
  await ui.flush();
  ui.value.setDraft("A-thread", "newer draft");
  written.resolve();
  assert.equal(await preparing, false);
  assert.equal(ui.value.draft("A-thread"), "newer draft");
  assert.equal(ui.value.getLifecycleState().dirty, true);
  host.handlers.delete("conversation_draft");
  assert.equal(await ui.value.prepareSuspend(), true);
  assert.equal(host.data.projects[0].conversations[0].draft, "newer draft");
});

test("failed source verification retains a clean buffer's error and refuses release", async (t) => {
  const { host, ui } = await fixture(t);
  await ui.value.loadFile("src/Main.java");
  await ui.flush();
  host.handlers.set("file_read", async () => {
    throw new Error("verification unavailable");
  });
  await assert.rejects(
    ui.value.saveFile("src/Main.java"),
    /verification unavailable/,
  );
  assert.equal(
    ui.value.getLifecycleState().dirty,
    true,
    "A failed verification is unresolved work even if content matched the old baseline",
  );
  assert.equal(await ui.value.prepareSuspend(), false);
  assert.ok(ui.value.buffer("src/Main.java"));
  assert.equal(ui.value.getLifecycleState().suspended, false);
});

test("new public mutations during autosave invalidate its generation fence", async (t) => {
  const { host, ui } = await fixture(t),
    saved = deferred();
  await openSource(ui, "source edit");
  host.handlers.set("file_save", async (args) => {
    await saved.promise;
    host.files.set("A:src/Main.java", {
      content: args.content,
      sha256: "saved-hash",
    });
    return host.commit();
  });
  const preparing = ui.value.prepareSuspend();
  await ui.flush();
  const mutation = ui.value.write("layout_save", {
    projectId: "B",
    layout: host.data.projects[1].layout,
  });
  saved.resolve();
  await mutation;
  assert.equal(await preparing, false);
  assert.equal(ui.value.getLifecycleState().suspended, false);
  assert.equal(await ui.value.prepareSuspend(), true);
});

test("concurrent suspension requests cannot bypass a caller's visibility fence", async (t) => {
  const { host, ui } = await fixture(t),
    saved = deferred();
  await openSource(ui, "source edit");
  host.handlers.set("file_save", async (args) => {
    await saved.promise;
    host.files.set("A:src/Main.java", {
      content: args.content,
      sha256: "saved-hash",
    });
    return host.commit();
  });
  const preparing = ui.value.prepareSuspend();
  assert.equal(await ui.value.prepareSuspend(() => false), false);
  saved.resolve();
  assert.equal(await preparing, true);
});

test("late context acknowledgement preserves newer edits and the original project owner", async (t) => {
  const { host, ui } = await fixture(t),
    saved = deferred();
  ui.value.setConversationContext("A-thread", {
    contextNote: "submitted",
    contextPaths: ["src/Main.java"],
  });
  host.handlers.set("conversation_context", async (args) => {
    await saved.promise;
    assert.equal(args.projectId, "A");
    assert.equal(args.conversationId, "A-thread");
    Object.assign(host.data.projects[0].conversations[0], {
      contextNote: args.contextNote,
      contextPaths: args.contextPaths,
    });
    return host.commit();
  });
  const request = ui.value.saveConversationContext("A", "A-thread");
  assert.equal(ui.value.getLifecycleState().busy, 1);
  await ui.flush();
  ui.value.setConversationContext("A-thread", {
    contextNote: "newer typed note",
    contextPaths: ["src/Main.java", "README.md"],
  });
  ui.value.choose(ui.value.snapshot.projects[1]);
  await ui.flush();
  ui.value.setConversationContext("B-thread", {
    contextNote: "B goal",
    contextPaths: [],
  });
  saved.resolve();
  await request;
  await ui.flush();
  assert.equal(ui.value.projectId, "B");
  assert.equal(ui.value.conversationContext("B-thread").contextNote, "B goal");
  ui.value.choose(ui.value.snapshot.projects[0]);
  await ui.flush();
  assert.deepEqual(ui.value.conversationContext("A-thread"), {
    contextNote: "newer typed note",
    contextPaths: ["src/Main.java", "README.md"],
  });
  assert.equal(ui.value.contextDirty("A-thread"), true);
  host.handlers.delete("conversation_context");
  await ui.value.saveConversationContext("A", "A-thread");
  await ui.flush();
  assert.equal(ui.value.contextDirty("A-thread"), false);
  assert.equal(
    host.data.projects[0].conversations[0].contextNote,
    "newer typed note",
  );
  assert.equal(host.data.projects[1].conversations[0].contextNote, "");
});

test("CAS refresh retains context and rename identity, then explicit retry uses the new revision", async (t) => {
  const { host, ui } = await fixture(t);
  const identity = host.data.projects[0].conversations[0];
  identity.messages.push({
    id: "earlier",
    role: "user",
    content: "Earlier request",
    jobId: null,
  });
  ui.value.setConversationContext("A-thread", {
    contextNote: "local goal",
    contextPaths: ["src/Main.java"],
  });
  ui.value.setConversationTitle("A-thread", "Local name");
  ui.value.setProjectInstructions("project conventions");
  host.commit();
  await assert.rejects(ui.value.saveConversationContext("A", "A-thread"));
  await ui.flush();
  assert.equal(ui.value.snapshot.revision, host.data.revision);
  assert.deepEqual(ui.value.conversationContext("A-thread"), {
    contextNote: "local goal",
    contextPaths: ["src/Main.java"],
  });
  assert.equal(ui.value.conversationTitle("A-thread"), "Local name");
  assert.equal(ui.value.projectInstructions(), "project conventions");
  await ui.value.saveConversationContext("A", "A-thread");
  await ui.value.saveConversationTitle("A", "A-thread");
  await ui.value.saveProjectInstructions("A");
  await ui.flush();
  assert.equal(identity.id, "A-thread");
  assert.equal(identity.title, "Local name");
  assert.deepEqual(identity.messages, [
    { id: "earlier", role: "user", content: "Earlier request", jobId: null },
  ]);
  assert.equal(identity.activeJob, null);
  assert.deepEqual(identity.contextPaths, ["src/Main.java"]);
  assert.equal(ui.value.getLifecycleState().dirty, false);
  ui.close();
  ui.remount();
  await ui.flush();
  assert.deepEqual(ui.value.conversationContext("A-thread"), {
    contextNote: "local goal",
    contextPaths: ["src/Main.java"],
  });
  assert.equal(ui.value.conversationTitle("A-thread"), "Local name");
  assert.equal(ui.value.projectInstructions(), "project conventions");
});

test("late rename and project-instruction saves cannot acknowledge newer typed metadata", async (t) => {
  const { host, ui } = await fixture(t),
    renamed = deferred(),
    instructed = deferred();
  ui.value.setConversationTitle("A-thread", "submitted name");
  ui.value.setProjectInstructions("submitted conventions");
  host.handlers.set("conversation_rename", async (args) => {
    await renamed.promise;
    host.data.projects[0].conversations[0].title = args.title;
    return host.commit();
  });
  host.handlers.set("project_instructions", async (args) => {
    await instructed.promise;
    host.data.projects[0].instructions = args.instructions;
    return host.commit();
  });
  const name = ui.value.saveConversationTitle("A", "A-thread"),
    instructions = ui.value.saveProjectInstructions("A");
  await ui.flush();
  ui.value.setConversationTitle("A-thread", "newer name");
  ui.value.setProjectInstructions("newer conventions");
  ui.value.choose(ui.value.snapshot.projects[1]);
  await ui.flush();
  renamed.resolve();
  await name;
  await ui.flush();
  instructed.resolve();
  await instructions;
  await ui.flush();
  assert.equal(ui.value.projectInstructions(), "");
  ui.value.choose(ui.value.snapshot.projects[0]);
  await ui.flush();
  assert.equal(ui.value.conversationTitle("A-thread"), "newer name");
  assert.equal(ui.value.projectInstructions(), "newer conventions");
  assert.equal(ui.value.titleDirty("A-thread"), true);
  assert.equal(ui.value.instructionsDirty(), true);
});

test("suspension persists context owned by hidden conversations and every project", async (t) => {
  const { host, ui } = await fixture(t);
  ui.value.setConversationContext("A-thread", {
    contextNote: "A goal",
    contextPaths: ["src/Main.java"],
  });
  ui.value.setConversationTitle("A-thread", "A name");
  ui.value.setProjectInstructions("A conventions");
  ui.value.choose(ui.value.snapshot.projects[1]);
  await ui.flush();
  ui.value.setConversationContext("B-thread", {
    contextNote: "B goal",
    contextPaths: [],
  });
  ui.value.setProjectInstructions("B conventions");
  assert.equal(ui.value.getLifecycleState().dirty, true);
  assert.equal(await ui.value.prepareSuspend(), true);
  assert.equal(host.data.projects[0].conversations[0].contextNote, "A goal");
  assert.equal(host.data.projects[0].conversations[0].title, "A name");
  assert.equal(host.data.projects[1].conversations[0].contextNote, "B goal");
  assert.deepEqual(
    host.data.projects.map((project) => project.instructions),
    ["A conventions", "B conventions"],
  );
  assert.equal(ui.value.getLifecycleState().dirty, false);
});

test("Harness admission persists selected context first and rejects context typed during preparation", async (t) => {
  const { host, ui } = await fixture(t);
  ui.value.setConversationContext("A-thread", {
    contextNote: "captured goal",
    contextPaths: ["src/Main.java"],
  });
  ui.value.setConversationTitle("A-thread", "captured name");
  ui.value.setProjectInstructions("captured conventions");
  let admitted = 0;
  host.handlers.set("rh_code_run", async () => {
    admitted++;
    assert.equal(
      host.data.projects[0].conversations[0].contextNote,
      "captured goal",
    );
    assert.equal(host.data.projects[0].instructions, "captured conventions");
    throw new Error("end isolated capture");
  });
  await ui.value.run("A-chat", "A-thread", "ask", "inspect");
  assert.equal(admitted, 1);
  const saved = deferred();
  await openSource(ui, "source edit");
  host.handlers.set("file_save", async (args) => {
    await saved.promise;
    host.files.set("A:src/Main.java", {
      content: args.content,
      sha256: "saved-source",
    });
    return host.commit();
  });
  const request = ui.value.run("A-chat", "A-thread", "ask", "inspect again");
  await ui.flush();
  ui.value.setConversationContext("A-thread", {
    contextNote: "typed while preparing",
    contextPaths: [],
  });
  saved.resolve();
  await request;
  await ui.flush();
  assert.equal(admitted, 1);
  assert.equal(ui.value.contextDirty("A-thread"), true);
  assert.match(ui.value.error, /Context changed while preparing/);
});

test("late metadata failure keeps its owner's draft without showing another project's error", async (t) => {
  const { host, ui } = await fixture(t),
    saved = deferred();
  ui.value.setConversationContext("A-thread", {
    contextNote: "retain A note",
    contextPaths: [],
  });
  host.handlers.set("conversation_context", async () => saved.promise);
  const request = ui.value.saveConversationContext("A", "A-thread");
  await ui.flush();
  ui.value.choose(ui.value.snapshot.projects[1]);
  await ui.flush();
  saved.reject(new Error("A-only save failure"));
  await assert.rejects(request, /A-only save failure/);
  await ui.flush();
  assert.equal(ui.value.projectId, "B");
  assert.equal(ui.value.error, "");
  assert.equal(ui.value.contextDirty("B-thread"), false);
  ui.value.choose(ui.value.snapshot.projects[0]);
  await ui.flush();
  assert.equal(
    ui.value.conversationContext("A-thread").contextNote,
    "retain A note",
  );
  assert.equal(ui.value.contextDirty("A-thread"), true);
  host.handlers.delete("conversation_context");
});

test("accepted context metadata drains after unmount without refilling a remounted controller", async (t) => {
  const { host, ui } = await fixture(t),
    saved = deferred();
  ui.value.setConversationContext("A-thread", {
    contextNote: "accepted A note",
    contextPaths: ["src/Main.java"],
  });
  host.handlers.set("conversation_context", async (args) => {
    await saved.promise;
    Object.assign(host.data.projects[0].conversations[0], {
      contextNote: args.contextNote,
      contextPaths: args.contextPaths,
    });
    return host.commit();
  });
  const previous = ui.value,
    request = previous.saveConversationContext("A", "A-thread");
  await ui.flush();
  ui.close();
  ui.remount();
  await ui.flush();
  ui.value.setConversationContext("A-thread", {
    contextNote: "new service draft",
    contextPaths: [],
  });
  saved.resolve();
  await request;
  await ui.flush();
  assert.equal(
    ui.value.conversationContext("A-thread").contextNote,
    "new service draft",
  );
  assert.equal(
    host.data.projects[0].conversations[0].contextNote,
    "accepted A note",
  );
  assert.equal(previous.getLifecycleState().busy, 0);
  assert.equal(previous.conversationContext("A-thread").contextNote, "");
  host.handlers.delete("conversation_context");
});
