/** Real Harness view hooks and review events against isolated fixtures.
 * No desktop, native files, runtime, provider or credentials are used. */
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync, existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url),
  ts = require("typescript"),
  source = fileURLToPath(new URL("../src/rh-code/", import.meta.url));
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
function driver(file, exported, initial, controller, language = "en") {
  const modules = new Map(),
    hooks = [];
  let cursor = 0,
    dirty = false,
    effects = [],
    value,
    props = initial,
    closed = false;
  const react = {
    Fragment: require("react").Fragment,
    useState(initial) {
      const index = cursor++;
      if (!(index in hooks))
        hooks[index] = typeof initial === "function" ? initial() : initial;
      return [
        hooks[index],
        (next) => {
          const value = typeof next === "function" ? next(hooks[index]) : next;
          if (!Object.is(value, hooks[index])) {
            hooks[index] = value;
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
        deps.some((value, index) => !Object.is(value, previous.deps[index]))
      ) {
        hooks[index] = { deps };
        effects.push(() => {
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
    function localRequire(name) {
      if (name === "react") return react;
      if (name.endsWith("/controller")) return { useCode: () => controller };
      if (name.endsWith("/locale") || name === "./locale")
        return { label: (zh, en) => (language === "en" ? en : zh) };
      if (name.endsWith("experimentalTypes"))
        return {
          experimentalCall: (api, operation, args) =>
            api("experimental_call", { operation, args }),
        };
      if (name.endsWith("CeSelect"))
        return { CeSelect: function CeSelect() {} };
      if (name.endsWith("MakerChangeSet"))
        return { MakerChangeSet: function MakerChangeSet() {} };
      if (name.startsWith(".")) {
        const base = path.resolve(path.dirname(file), name);
        return load(existsSync(base + ".tsx") ? base + ".tsx" : base + ".ts");
      }
      return require(name);
    }
    new Function("require", "module", "exports", code)(
      localRequire,
      module,
      module.exports,
    );
    return module.exports;
  }
  const component = load(path.join(source, file))[exported];
  function render(next = props) {
    props = next;
    cursor = 0;
    dirty = false;
    effects = [];
    value = component(props);
    for (const effect of effects) effect();
  }
  render();
  return {
    get value() {
      return value;
    },
    module: (file) => load(path.join(source, file)),
    render,
    async flush() {
      for (let index = 0; index < 30; index++) {
        await new Promise((done) => setImmediate(done));
        if (closed || !dirty) return;
        render();
      }
      throw new Error("View did not settle");
    },
    close() {
      closed = true;
      for (const hook of hooks) hook?.cleanup?.();
    },
  };
}
function elements(node) {
  if (!node || typeof node !== "object") return [];
  if (Array.isArray(node)) return node.flatMap(elements);
  return [node, ...elements(node.props?.children)];
}
function text(node) {
  if (node === null || node === undefined || typeof node === "boolean")
    return "";
  if (typeof node !== "object") return String(node);
  if (Array.isArray(node)) return node.map(text).join("");
  return text(node.props?.children);
}
const job = (id, status = "completed", revision = 1) => ({
  summary: {
    jobId: id,
    revision,
    workflow: "harness",
    status,
    mode: "live",
    updatedAt: "2026-10-12T00:00:00.000Z",
    artifactState: "none",
    blockedReason: null,
  },
  mode: "execute",
  result: null,
  artifacts: [],
  reviews: [],
});
const conversation = (id, activeJob = null, ids = []) => ({
  id,
  title: id,
  draft: "Request",
  mode: "execute",
  activeJob,
  messages: ids.map((jobId, index) => ({
    id: id + index,
    role: "assistant",
    content: "Result",
    jobId,
  })),
});
const project = (id, conversations) => ({
  id,
  name: id,
  folder: "/fixture",
  targets: [],
  conversations,
});
const proposal = (id = "proposal", digest = "digest") => ({
  id,
  digest,
  status: "pending",
  summary: "Review the change",
  changes: [
    {
      path: "src/Main.java",
      kind: "modify",
      beforeSha256: "before",
      afterSha256: "after",
      before: "old\n",
      after: "new\n",
    },
  ],
});
const controller = () => ({
  api: async () => {
    throw new Error("Unexpected host call");
  },
  native: true,
  projectId: "first",
  project: project("first", []),
  jobs: {},
  busy: 0,
  engine: null,
  readHarnessProposals: async () => ({ proposals: [] }),
  applyHarnessProposal: async () => {},
  openAiSettings() {},
  setError() {},
  draft: () => "Request",
  setDraft() {},
  patchArea() {},
  run: async () => {},
  stop: async () => {},
});

test("diff retains changed lines and trims unchanged context; only visible text is projected", () => {
  const view = driver(
    "plugins/JobActivity.tsx",
    "JobActivity",
    { job: job("one"), jobId: "one" },
    controller(),
  );
  const { changeDiff, jobText, modeDescription } =
    view.module("harnessView.ts");
  const diff = changeDiff({
    ...proposal().changes[0],
    before: "a\nb\nc\nd\ne\nf\ng\nh",
    after: "a\nb\nc\nd\nNEW\nf\ng\nh",
  });
  assert.equal(diff.header, "@@ -2,7 +2,7 @@");
  assert.deepEqual(
    diff.lines
      .filter((line) => line.kind !== "context")
      .map(({ kind, text }) => ({ kind, text })),
    [
      { kind: "remove", text: "e" },
      { kind: "add", text: "NEW" },
    ],
  );
  assert.equal(
    changeDiff({ before: null, after: "created" }).lines[0].kind,
    "add",
  );
  assert.equal(
    changeDiff({ before: "deleted", after: null }).lines[0].kind,
    "remove",
  );
  const newline = changeDiff({ before: "last", after: "last\n" });
  assert.equal(newline.header, "@@ -1,1 +1,1 @@");
  assert.deepEqual(newline.lines, [
    { kind: "remove", text: "last", noNewline: true },
    { kind: "add", text: "last", noNewline: false },
  ]);
  assert.equal(
    jobText({
      ...job("one"),
      output: "Visible text",
      result: { privateProviderState: "hidden" },
    }),
    "Visible text",
  );
  assert.equal(
    jobText({ ...job("one"), result: { text: "Final" }, output: "Partial" }),
    "Final",
  );
  assert.equal(modeDescription("plan"), "Read-only analysis and planning");
  view.close();
});

test("historical replies retire on project switches and fresh live receipts win", async () => {
  const c = controller(),
    pending = deferred();
  c.project = project("first", [conversation("thread", null, ["first-job"])]);
  c.api = async (_command, { args }) =>
    args.jobId === "first-job" ? pending.promise : job(args.jobId);
  const view = driver(
    "plugins/useProjectJobs.ts",
    "useProjectJobs",
    undefined,
    c,
  );
  c.projectId = "second";
  c.project = project("second", [
    conversation("second-thread", null, ["second-job"]),
  ]);
  view.render();
  await view.flush();
  pending.resolve(job("first-job"));
  await view.flush();
  assert.deepEqual(
    view.value.jobs.map((value) => value.summary.jobId),
    ["second-job"],
  );
  c.jobs["second-job"] = { ...job("second-job", "running", 2), output: "Live" };
  view.render();
  assert.equal(view.value.jobs[0].output, "Live");
  view.close();
});

test("bounded history preserves older active jobs and validates task IDs", async () => {
  const c = controller();
  c.project = project("first", [
    conversation("old", "active"),
    conversation(
      "new",
      null,
      Array.from({ length: 15 }, (_, index) => "history-" + index),
    ),
  ]);
  c.api = async (_command, { args }) =>
    args.jobId === "history-14" ? job("wrong-job") : job(args.jobId);
  const view = driver(
    "plugins/useProjectJobs.ts",
    "useProjectJobs",
    undefined,
    c,
  );
  await view.flush();
  assert.ok(view.value.jobs.some((value) => value.summary.jobId === "active"));
  assert.match(view.value.error, /identity mismatch/);
  assert.ok(
    !view.value.jobs.some((value) => value.summary.jobId === "wrong-job"),
  );
  view.close();
});

test("review applies finished pending proposal with displayed job/id/digest", async () => {
  const c = controller(),
    calls = [];
  c.readHarnessProposals = async () => ({ proposals: [proposal()] });
  c.applyHarnessProposal = async (...args) => {
    calls.push(args);
    c.readHarnessProposals = async () => ({
      proposals: [{ ...proposal(), status: "applied" }],
    });
  };
  const view = driver(
    "plugins/HarnessProposalReview.tsx",
    "HarnessProposalReview",
    { job: job("captured-job", "running") },
    c,
  );
  await view.flush();
  const findApply = () =>
    elements(view.value).find(
      (element) =>
        element.type === "button" &&
        text(element).includes("Approve and apply"),
    );
  assert.equal(findApply().props.disabled, true);
  view.render({ job: job("captured-job", "completed", 2) });
  await view.flush();
  assert.equal(findApply().props.disabled, false);
  findApply().props.onClick();
  await view.flush();
  assert.deepEqual(calls, [["captured-job", "proposal", "digest"]]);
  assert.match(text(view.value), /Applied to project/);
  view.close();
});

test("late proposal read and apply cannot refill another project", async () => {
  const c = controller(),
    lateRead = deferred(),
    lateApply = deferred();
  c.readHarnessProposals = async () => lateRead.promise;
  const view = driver(
    "plugins/HarnessProposalReview.tsx",
    "HarnessProposalReview",
    { job: job("first-job") },
    c,
  );
  c.projectId = "second";
  c.readHarnessProposals = async () => ({
    proposals: [proposal("second", "second-digest")],
  });
  c.applyHarnessProposal = async () => lateApply.promise;
  view.render({ job: job("second-job") });
  await view.flush();
  lateRead.resolve({ proposals: [proposal("old")] });
  await view.flush();
  elements(view.value)
    .find(
      (element) =>
        element.type === "button" &&
        text(element).includes("Approve and apply"),
    )
    .props.onClick();
  await view.flush();
  c.projectId = "third";
  c.readHarnessProposals = async () => ({ proposals: [] });
  view.render({ job: job("third-job") });
  await view.flush();
  lateApply.reject(new Error("Old conflict"));
  await view.flush();
  assert.equal(view.value, null);
  view.close();
});

test("apply failure retains the reviewed source and reports the conflict", async () => {
  const c = controller();
  c.readHarnessProposals = async () => ({ proposals: [proposal()] });
  c.applyHarnessProposal = async () => {
    throw new Error("Source changed on disk");
  };
  const view = driver(
    "plugins/HarnessProposalReview.tsx",
    "HarnessProposalReview",
    { job: job("one") },
    c,
  );
  await view.flush();
  elements(view.value)
    .find(
      (element) =>
        element.type === "button" &&
        text(element).includes("Approve and apply"),
    )
    .props.onClick();
  await view.flush();
  assert.match(text(view.value), /Source changed on disk/);
  assert.match(text(view.value), /src\/Main.java/);
  assert.match(text(view.value), /old/);
  assert.match(text(view.value), /new/);
  view.close();
});

test("unsuccessful jobs and incomplete file lists cannot expose an enabled apply action", async () => {
  for (const state of [
    "queued",
    "running",
    "cancelled",
    "failed",
    "interrupted",
    "completed",
  ]) {
    const c = controller();
    c.readHarnessProposals = async () => ({
      proposals: [{ ...proposal(), changesTruncated: state === "completed" }],
      truncated: state === "completed",
    });
    const view = driver(
      "plugins/HarnessProposalReview.tsx",
      "HarnessProposalReview",
      { job: job("one", state) },
      c,
    );
    await view.flush();
    const apply = elements(view.value).find(
      (element) =>
        element.type === "button" &&
        text(element).includes("Approve and apply"),
    );
    assert.equal(apply.props.disabled, true, state);
    if (state === "completed") {
      assert.match(text(view.value), /file list is incomplete/);
      assert.match(text(view.value), /Some proposals are not shown/);
    }
    view.close();
  }
});

test("activity displays tools/output/errors and prevents repeated stop", async () => {
  const pending = deferred(),
    calls = [],
    value = {
      ...job("one", "running"),
      output: "Visible model reply",
      activity: [
        {
          id: "tool",
          name: "harness.report_plan",
          status: "completed",
          steps: ["Read source", "Propose changes"],
        },
        {
          id: "read",
          name: "harness.read",
          status: "running",
          path: "src/Main.java",
        },
      ],
    };
  const view = driver(
    "plugins/JobActivity.tsx",
    "JobActivity",
    {
      job: value,
      jobId: "one",
      onStop: async (id) => {
        calls.push(id);
        return pending.promise;
      },
    },
    controller(),
  );
  assert.match(text(view.value), /Visible model reply/);
  assert.match(text(view.value), /harness.read/);
  assert.match(text(view.value), /Read source/);
  elements(view.value)
    .find((element) => element.type === "button")
    .props.onClick();
  await view.flush();
  assert.match(text(view.value), /Stopping/);
  assert.equal(
    elements(view.value).find((element) => element.type === "button").props
      .disabled,
    true,
  );
  assert.deepEqual(calls, ["one"]);
  pending.resolve();
  await view.flush();
  view.render({
    job: {
      ...job("one", "failed", 2),
      error: { message: "Provider request failed" },
    },
    jobId: "one",
  });
  await view.flush();
  assert.match(text(view.value), /Provider request failed/);
  assert.equal(
    elements(view.value).filter((element) => element.type === "button").length,
    0,
  );
  view.close();
});

test("selected saved preset admits send after sleep; missing preset offers API setup", async () => {
  const c = controller();
  c.project = project("first", [conversation("thread")]);
  c.engine = {
    liveConfigured: false,
    liveAvailable: true,
    aiPresets: { selectedId: "saved", presets: [{ id: "saved" }] },
  };
  const view = driver(
    "plugins/conversations.tsx",
    "Chat",
    { area: { id: "area", conversation: "thread" } },
    c,
  );
  await view.flush();
  const send = () =>
    elements(view.value).find(
      (element) =>
        element.type === "button" &&
        element.props["aria-label"] === "Send request",
    );
  assert.equal(send().props.disabled, false);
  c.engine = {
    ...c.engine,
    liveAvailable: false,
    aiPresets: { selectedId: null, presets: [] },
  };
  view.render();
  assert.equal(send().props.disabled, true);
  assert.match(text(view.value), /Propose changes for review/);
  const notice = elements(view.value).find(
    (element) => element.type?.name === "RuntimeNotice",
  );
  assert.match(text(notice.type(notice.props)), /Configure API/);
  assert.match(text(notice.type(notice.props)), /Configure an AI preset/);
  view.close();
});

test("task completion preserves transcript scroll when the user is reading earlier messages", async () => {
  const c = controller();
  c.project = project("first", [conversation("thread", "one")]);
  c.jobs.one = job("one", "running");
  const view = driver(
    "plugins/conversations.tsx",
    "Chat",
    { area: { id: "area", conversation: "thread" } },
    c,
  );
  await view.flush();
  const transcript = elements(view.value).find(
      (element) => element.props?.className === "rc-messages",
    ),
    node = { scrollHeight: 500, scrollTop: 100, clientHeight: 100 };
  transcript.props.ref.current = node;
  transcript.props.onScroll({ currentTarget: node });
  c.jobs.one = job("one", "completed", 2);
  c.project.conversations[0].activeJob = null;
  view.render();
  await view.flush();
  assert.equal(node.scrollTop, 100);
  view.close();
});

function metadataController() {
  const c = controller(),
    calls = [],
    contexts = new Map(),
    titles = new Map(),
    instructions = new Map(),
    dirty = new Set();
  const p = project("first", [conversation("thread")]);
  p.instructions = "Shared convention";
  p.conversations[0].contextNote = "Saved goal";
  p.conversations[0].contextPaths = ["src/Main.java"];
  c.project = p;
  c.snapshot = { disabledPlugins: [], projects: [p] };
  const key = (id) => c.projectId + ":" + id;
  c.conversationContext = (id) =>
    contexts.get(key(id)) ?? {
      contextNote:
        c.project.conversations.find((value) => value.id === id)?.contextNote ??
        "",
      contextPaths:
        c.project.conversations.find((value) => value.id === id)
          ?.contextPaths ?? [],
    };
  c.conversationTitle = (id) =>
    titles.get(key(id)) ??
    c.project.conversations.find((value) => value.id === id)?.title ??
    "";
  c.projectInstructions = () =>
    instructions.get(c.projectId) ?? c.project.instructions ?? "";
  c.contextDirty = (id) => dirty.has("context:" + key(id));
  c.titleDirty = (id) => dirty.has("title:" + key(id));
  c.instructionsDirty = () => dirty.has("instructions:" + c.projectId);
  c.setConversationContext = (id, value) => {
    contexts.set(key(id), value);
    dirty.add("context:" + key(id));
  };
  c.setConversationTitle = (id, value) => {
    titles.set(key(id), value);
    dirty.add("title:" + key(id));
  };
  c.setProjectInstructions = (value) => {
    instructions.set(c.projectId, value);
    dirty.add("instructions:" + c.projectId);
  };
  c.saveConversationContext = async (owner, id) => {
    calls.push({
      operation: "conversation_context",
      owner,
      id,
      value: structuredClone(contexts.get(owner + ":" + id)),
    });
    dirty.delete("context:" + owner + ":" + id);
  };
  c.saveConversationTitle = async (owner, id) => {
    calls.push({
      operation: "conversation_rename",
      owner,
      id,
      value: titles.get(owner + ":" + id),
    });
    dirty.delete("title:" + owner + ":" + id);
  };
  c.saveProjectInstructions = async (owner) => {
    calls.push({
      operation: "project_instructions",
      owner,
      value: instructions.get(owner),
    });
    dirty.delete("instructions:" + owner);
  };
  c.api = async (_command, { operation, args }) => {
    calls.push({ operation, args });
    if (operation === "files") return { files: ["src/Main.java", "README.md"] };
    if (operation === "file_read") return { content: "fixture UTF-8 text" };
    throw new Error("Unexpected fixture operation");
  };
  return { c, calls };
}
const buttonNamed = (value, name) =>
  elements(value).find(
    (element) =>
      element.type === "button" &&
      (element.props["aria-label"] === name || text(element) === name),
  );
const inputNamed = (value, name) =>
  elements(value).find(
    (element) =>
      ["input", "textarea"].includes(element.type) &&
      element.props["aria-label"] === name,
  );

test("saved references restore as chips and context stays folded with bounded editors in both locales", async () => {
  for (const language of ["en", "zh"]) {
    const { c } = metadataController();
    const view = driver(
      "plugins/ConversationContext.tsx",
      "ConversationContext",
      { conversation: c.project.conversations[0] },
      c,
      language,
    );
    assert.match(text(view.value), /src\/Main.java/);
    assert.equal(
      elements(view.value).filter((element) => element.type === "textarea")
        .length,
      0,
    );
    buttonNamed(
      view.value,
      language === "en" ? "Context·" : "上下文·",
    ).props.onClick();
    await view.flush();
    const editors = elements(view.value).filter(
      (element) => element.type === "textarea",
    );
    assert.deepEqual(
      editors.map((element) => element.props.value),
      ["Saved goal", "Shared convention"],
    );
    assert.ok(editors.every((element) => element.props.maxLength === 8000));
    assert.ok(
      text(view.value).length < 200,
      "Context controls must keep their copy compact",
    );
    view.close();
  }
});

test("file references use a bounded searchable inventory and preserve a note typed during text verification", async () => {
  const { c, calls } = metadataController(),
    reading = deferred();
  c.api = async (_command, { operation, args }) => {
    calls.push({ operation, args });
    if (operation === "files")
      return {
        files: [
          ...Array.from(
            { length: 60 },
            (_, index) => "src/File" + index + ".java",
          ),
          "src/Main.java",
          "vendor/secret.txt",
          "venv/private.txt",
          ".secret",
          "src/.hidden",
          "../escape",
          "src/invalid:name",
          "src/" + "中".repeat(180),
        ],
      };
    if (operation === "file_read") return reading.promise;
  };
  const view = driver(
    "plugins/ConversationContext.tsx",
    "ConversationContext",
    { conversation: c.project.conversations[0] },
    c,
  );
  buttonNamed(view.value, "Reference project files").props.onClick();
  await view.flush();
  assert.equal(
    elements(view.value).filter(
      (element) =>
        element.type === "button" &&
        element.props["aria-pressed"] !== undefined,
    ).length,
    40,
  );
  assert.match(text(view.value), /Type more to narrow/);
  inputNamed(view.value, "Search project files").props.onChange({
    target: { value: "File59" },
  });
  await view.flush();
  assert.match(text(view.value), /src\/File59.java/);
  const candidate = elements(view.value).find(
    (element) =>
      element.type === "button" && element.props.title === "src/File59.java",
  );
  candidate.props.onClick();
  await view.flush();
  c.setConversationContext("thread", {
    contextNote: "newer goal during verification",
    contextPaths: ["src/Main.java"],
  });
  reading.resolve({ content: "safe text" });
  await view.flush();
  assert.deepEqual(
    calls.find((call) => call.operation === "conversation_context").value,
    {
      contextNote: "newer goal during verification",
      contextPaths: ["src/Main.java", "src/File59.java"],
    },
  );
  const { contextReferencePaths } = view.module(
    "plugins/ConversationContext.tsx",
  );
  assert.deepEqual(
    contextReferencePaths([
      "README.md",
      "README.md",
      "src/Main.java",
      "vendor/secret.txt",
      "venv/a",
      "__pycache__/a",
      ".secret",
      "../escape",
      "/absolute",
      "src/invalid:name",
      "src/" + "中".repeat(180),
    ]),
    ["README.md", "src/Main.java"],
  );
  view.close();
});

test("twenty references disable more selections and chip removal saves the same conversation", async () => {
  const { c, calls } = metadataController();
  c.project.conversations[0].contextPaths = Array.from(
    { length: 20 },
    (_, index) => "src/File" + index + ".java",
  );
  const view = driver(
    "plugins/ConversationContext.tsx",
    "ConversationContext",
    { conversation: c.project.conversations[0] },
    c,
  );
  buttonNamed(view.value, "Reference project files").props.onClick();
  await view.flush();
  assert.equal(buttonNamed(view.value, "src/Main.java").props.disabled, true);
  buttonNamed(view.value, "Remove reference · src/File0.java").props.onClick();
  await view.flush();
  assert.equal(calls.at(-1).owner, "first");
  assert.equal(calls.at(-1).id, "thread");
  assert.equal(calls.at(-1).value.contextPaths.length, 19);
  assert.equal(calls.at(-1).value.contextNote, "Saved goal");
  view.close();
});

test("late inventory and selected-file replies cannot attach files or errors to another project", async () => {
  for (const delayed of ["inventory", "text"]) {
    const { c, calls } = metadataController(),
      late = deferred();
    c.api = async (_command, { operation }) => {
      if (operation === "files")
        return delayed === "inventory" && c.projectId === "first"
          ? late.promise
          : {
              files: [
                c.projectId === "first" ? "src/Old.java" : "src/New.java",
              ],
            };
      if (operation === "file_read") return late.promise;
    };
    const view = driver(
      "plugins/ConversationContext.tsx",
      "ConversationContext",
      { conversation: c.project.conversations[0] },
      c,
    );
    buttonNamed(view.value, "Reference project files").props.onClick();
    await view.flush();
    if (delayed === "text") {
      buttonNamed(view.value, "src/Old.java").props.onClick();
      await view.flush();
    }
    c.projectId = "second";
    c.project = project("second", [conversation("thread")]);
    view.render({ conversation: c.project.conversations[0] });
    await view.flush();
    buttonNamed(view.value, "Reference project files").props.onClick();
    await view.flush();
    late.resolve(
      delayed === "inventory"
        ? { files: ["src/Old.java"] }
        : { content: "late old text" },
    );
    await view.flush();
    assert.doesNotMatch(text(view.value), /src\/Old.java/);
    assert.match(text(view.value), /src\/New.java/);
    assert.equal(
      calls.filter((call) => call.operation === "conversation_context").length,
      0,
    );
    view.close();
  }
});

test("non-text reference checks retain saved context and surface the selected file error", async () => {
  const { c, calls } = metadataController();
  c.api = async (_command, { operation }) =>
    operation === "files"
      ? { files: ["binary.dat"] }
      : { content: "binary\0bytes" };
  const view = driver(
    "plugins/ConversationContext.tsx",
    "ConversationContext",
    { conversation: c.project.conversations[0] },
    c,
  );
  buttonNamed(view.value, "Reference project files").props.onClick();
  await view.flush();
  buttonNamed(view.value, "binary.dat").props.onClick();
  await view.flush();
  assert.match(text(view.value), /Choose a UTF-8 text file/);
  assert.deepEqual(c.conversationContext("thread").contextPaths, [
    "src/Main.java",
  ]);
  assert.equal(calls.length, 0);
  view.close();
});

test("rename saves the existing conversation and retires late feedback after a project switch", async () => {
  const { c, calls } = metadataController(),
    renamed = deferred();
  c.saveConversationTitle = async (owner, id) => {
    calls.push({
      operation: "conversation_rename",
      owner,
      id,
      value: c.conversationTitle(id),
    });
    return renamed.promise;
  };
  const view = driver(
    "plugins/ConversationContext.tsx",
    "ConversationName",
    { conversation: c.project.conversations[0] },
    c,
  );
  buttonNamed(view.value, "Rename conversation").props.onClick();
  await view.flush();
  inputNamed(view.value, "Conversation name").props.onChange({
    target: { value: "Renamed thread" },
  });
  view.render();
  buttonNamed(view.value, "Save name").props.onClick();
  await view.flush();
  assert.deepEqual(calls[0], {
    operation: "conversation_rename",
    owner: "first",
    id: "thread",
    value: "Renamed thread",
  });
  c.projectId = "second";
  c.project = project("second", [conversation("thread")]);
  view.render({ conversation: c.project.conversations[0] });
  await view.flush();
  renamed.reject(new Error("old rename failed"));
  await view.flush();
  assert.doesNotMatch(text(view.value), /old rename failed/);
  buttonNamed(view.value, "Rename conversation").props.onClick();
  await view.flush();
  assert.equal(
    inputNamed(view.value, "Conversation name").props.value,
    "thread",
  );
  view.close();
});

test("disabled changes never mount/read Harness review and disabled source prevents applying", async () => {
  const c = controller();
  c.snapshot = { disabledPlugins: ["rh.changes"] };
  c.project = project("first", [conversation("thread", null, ["one"])]);
  c.jobs.one = job("one");
  let reads = 0,
    applications = 0;
  c.readHarnessProposals = async () => {
    reads++;
    return { proposals: [proposal()] };
  };
  c.applyHarnessProposal = async () => {
    applications++;
  };
  const chat = driver(
    "plugins/conversations.tsx",
    "Chat",
    { area: { id: "area", conversation: "thread" } },
    c,
  );
  await chat.flush();
  assert.ok(
    !elements(chat.value).some(
      (element) => element.type?.name === "HarnessProposalReview",
    ),
  );
  const review = driver(
    "plugins/HarnessProposalReview.tsx",
    "HarnessProposalReview",
    { job: job("one") },
    c,
  );
  await review.flush();
  assert.equal(reads, 0);
  assert.equal(review.value, null);
  c.snapshot.disabledPlugins = ["rh.source"];
  review.render();
  await review.flush();
  const apply = elements(review.value).find(
    (element) =>
      element.type === "button" && text(element).includes("Approve and apply"),
  );
  assert.equal(apply.props.disabled, true);
  apply.props.onClick();
  await review.flush();
  assert.equal(applications, 0);
  chat.close();
  review.close();
});
