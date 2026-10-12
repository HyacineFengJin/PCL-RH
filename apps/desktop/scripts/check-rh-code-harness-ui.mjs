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
