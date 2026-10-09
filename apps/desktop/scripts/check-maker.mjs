import test from "node:test";
import assert from "node:assert/strict";
import {
  harness,
  elements,
  content,
  button,
  click,
  deferred,
  project,
  engine,
} from "./maker-test-harness.mjs";
const round = {
  jobId: "job-1",
  request: "Maintain a large mod",
  mode: "execute",
  checkpointId: "point-1",
  scopes: [],
};
const thread = {
  id: "thread-1",
  projectId: "project-1",
  name: "Example",
  notes: "",
  title: "Improve complex rules",
  createdAt: "2026-01-01T00:00:00.000Z",
  updatedAt: "2026-01-01T00:00:00.000Z",
  rounds: [round],
};
function fixture(records = [project()]) {
  const calls = [],
    intercept = new Map();
  let threads = [thread];
  const api = async (_, { operation, args }) => {
    calls.push({ operation, args });
    if (intercept.has(operation)) return intercept.get(operation)(args);
    if (operation === "projects_list") return { projects: records };
    if (operation === "status") return engine;
    if (operation === "maker_threads")
      return { threads, errors: [], pendingJobs: [] };
    if (operation === "maker_thread_read")
      return threads.find((t) => t.id === args.threadId);
    if (operation === "maker_thread_run") {
      const next = {
        ...thread,
        id: "new-thread",
        title: args.request,
        rounds: [
          {
            ...round,
            jobId: "new-job",
            request: args.request,
            mode: args.mode,
            scopes: args.scopes,
          },
        ],
      };
      threads.push(next);
      return next;
    }
    throw new Error(operation);
  };
  return { api, calls, intercept };
}
function openProject(ui, id = "project-1") {
  const tile = elements(
    ui.tree,
    (node) =>
      node.props?.className === "ma-project-card" && content(node).includes(id),
  )[0];
  assert.ok(tile);
  tile.props.onClick();
}
function input(ui) {
  return elements(
    ui.tree,
    (node) =>
      node.type === "textarea" &&
      node.props["aria-label"] === ui.i18n.t("agent.instruction"),
  )[0];
}
function submit(ui) {
  elements(
    ui.tree,
    (node) => node.props.className === "ma-composer",
  )[0].props.onSubmit({ preventDefault() {} });
}
test("project library stays bounded and contains summaries; workspace separates threads and graph", async () => {
  const f = fixture(
      Array.from({ length: 30 }, (_, i) => project(`project-${i + 1}`)),
    ),
    ui = harness("ExperimentalProjects", { api: f.api, native: true });
  await ui.flush();
  assert.equal(
    elements(ui.tree, (n) => n.props?.className === "ma-project-card").length,
    12,
  );
  assert.ok(!content(ui.tree).includes("Saved requirements"));
  openProject(ui);
  await ui.flush();
  assert.equal(
    elements(ui.tree, (n) => n.type?.name === "MakerGraph").length,
    1,
  );
  assert.equal(
    elements(ui.tree, (n) => n.props?.className === "ma-thread-main").length,
    1,
  );
  assert.equal(
    elements(ui.tree, (n) => n.type?.name === "MakerSource").length,
    0,
  );
  ui.close();
});
test("natural language submission captures project version and mode through the shared thread API", async () => {
  const f = fixture(),
    ui = harness("ExperimentalProjects", { api: f.api, native: true });
  await ui.flush();
  openProject(ui);
  await ui.flush();
  input(ui).props.onChange({
    target: { value: "/plan Design a multi-stage rule" },
  });
  await ui.flush();
  submit(ui);
  await ui.flush();
  const call = f.calls.find((c) => c.operation === "maker_thread_run");
  assert.equal(call.args.mode, "plan");
  assert.equal(call.args.checkpointId, "point-1");
  assert.equal(call.args.expectedRevision, "revision-1");
  assert.equal(call.args.request, "/plan Design a multi-stage rule");
  assert.equal(call.args.spec, undefined);
  assert.ok(
    !f.calls.some(
      (c) => c.operation === "provider_start" || c.operation === "job_create",
    ),
  );
  ui.close();
});
test("composer and scope drafts survive thread navigation; late thread reads cannot replace selection", async () => {
  const f = fixture(),
    slow = deferred();
  f.intercept.set("maker_thread_read", () => slow.promise);
  const ui = harness("ExperimentalProjects", { api: f.api, native: true });
  await ui.flush();
  openProject(ui);
  await ui.flush();
  const select = elements(ui.tree, (n) =>
    n.props?.className?.includes?.("ma-thread-entry"),
  )[0];
  select.props.onClick();
  await ui.flush();
  input(ui).props.onChange({ target: { value: "Unsaved requirement" } });
  await ui.flush();
  await click(ui, "agent.newThread");
  slow.resolve(thread);
  await ui.flush();
  assert.equal(
    elements(ui.tree, (n) => n.type?.name === "MakerThreadView").length,
    0,
  );
  assert.equal(input(ui).props.value, "");
  assert.equal(
    ui.props.draft.current.prompts.get("thread-1"),
    "Unsaved requirement",
  );
  ui.close();
});
test("review and port commands explain current boundaries without starting or validating anything", async () => {
  const f = fixture(),
    ui = harness("ExperimentalProjects", { api: f.api, native: true });
  await ui.flush();
  openProject(ui);
  await ui.flush();
  for (const value of ["/review", "/port"]) {
    input(ui).props.onChange({ target: { value } });
    await ui.flush();
    submit(ui);
    await ui.flush();
  }
  assert.ok(
    !f.calls.some(
      (c) =>
        c.operation === "maker_thread_run" ||
        c.operation === "build_validate" ||
        c.operation === "game_start",
    ),
  );
  assert.ok(content(ui.tree).includes(ui.i18n.t("agent.portCommand")));
  ui.close();
});
test("a retired send callback cannot admit a job after native ownership changes", async () => {
  const f = fixture(),
    ui = harness("ExperimentalProjects", { api: f.api, native: true });
  await ui.flush();
  openProject(ui);
  await ui.flush();
  input(ui).props.onChange({ target: { value: "Change behavior" } });
  await ui.flush();
  const saved = elements(
    ui.tree,
    (n) => n.props?.className === "ma-composer",
  )[0].props.onSubmit;
  ui.replace({ native: false });
  await ui.flush();
  saved({ preventDefault() {} });
  await ui.flush();
  assert.ok(!f.calls.some((c) => c.operation === "maker_thread_run"));
  ui.close();
});
test("source opens as a read-only overlay, and closing it cannot cancel a review", async () => {
  const f = fixture(),
    ui = harness("ExperimentalProjects", { api: f.api, native: true });
  await ui.flush();
  openProject(ui);
  await ui.flush();
  const graph = elements(ui.tree, (n) => n.type?.name === "MakerGraph")[0];
  graph.props.onSource("src/main/java/Rules.java");
  await ui.flush();
  const source = elements(ui.tree, (n) => n.type?.name === "MakerSource")[0];
  assert.equal(source.props.readOnly, true);
  assert.equal(source.props.initialPath, "src/main/java/Rules.java");
  const close = elements(
    ui.tree,
    (n) => n.props["aria-label"] === ui.i18n.t("agent.close"),
  )[0];
  close.props.onClick();
  await ui.flush();
  assert.ok(
    !f.calls.some(
      (c) => c.operation === "review_cancel" || c.operation === "review_apply",
    ),
  );
  ui.close();
});
test("unit confirmations gate one digest; uncertain application must refresh and archives recover without a replay", async () => {
  const calls = [],
    unit = {
      id: "unit-1",
      name: "Example blade",
      kind: "items",
      files: ["src/main/java/Blade.java"],
      notes: "",
    };
  let status = "pending",
    fail = true;
  const api = async (_, { operation, args }) => {
    calls.push({ operation, args });
    if (operation === "review_read")
      return {
        jobId: "job-1",
        reviewId: "review-1",
        reviewDigest: "digest-1",
        status,
        preview: {
          changes: [{ path: unit.files[0], unified_diff: "+ new behavior" }],
          locked_files: [],
        },
      };
    if (operation === "maker_thread_apply") {
      status = "applied";
      if (fail) {
        fail = false;
        throw new Error("Lost archive reply");
      }
      return {};
    }
    throw new Error(operation);
  };
  const ui = harness("MakerChangeSet", {
    api,
    native: true,
    threadId: "thread-1",
    jobId: "job-1",
    reviewId: "review-1",
    units: [unit],
    finished: true,
    onDiff() {},
    onFollowup() {},
    onCommitted: async () => {},
    onPending() {},
    onFiles() {},
  });
  await ui.flush();
  assert.equal(button(ui, "agent.applyArchive").props.disabled, true);
  await click(ui, "agent.approveUnit");
  assert.equal(button(ui, "agent.applyArchive").props.disabled, false);
  const apply = button(ui, "agent.applyArchive").props.onClick;
  apply();
  await ui.flush();
  apply();
  await ui.flush();
  assert.equal(
    calls.filter((c) => c.operation === "maker_thread_apply").length,
    1,
  );
  await click(ui, "maker.refreshReview");
  await click(ui, "agent.reconcileArchive");
  assert.equal(
    calls.filter((c) => c.operation === "maker_thread_apply").length,
    2,
  );
  assert.ok(
    calls
      .filter((c) => c.operation === "maker_thread_apply")
      .every((c) => c.args.digest === "digest-1"),
  );
  assert.ok(!calls.some((c) => c.operation === "review_cancel"));
  ui.close();
});
test("late @ dossier reads cannot add stale scope after query or connection changes", async () => {
  const slow = deferred(),
    chosen = [];
  const api = async (_, { operation }) =>
    operation === "project_units"
      ? {
          id: "project-1",
          revision: "revision-1",
          units: [{ id: "unit-1", kind: "items", name: "Blade" }],
        }
      : slow.promise;
  const ui = harness("MakerMentions", {
    api,
    native: true,
    project: project(),
    query: "B",
    onChoose: (value) => chosen.push(value),
  });
  await ui.flush();
  const pick = elements(ui.tree, (n) => n.type === "button")[0];
  pick.props.onClick();
  await ui.flush();
  ui.replace({ query: "Other" });
  await ui.flush();
  slow.resolve({
    id: "project-1",
    revision: "revision-1",
    unit: { id: "unit-1", name: "Blade", kind: "items", files: [], notes: "" },
  });
  await ui.flush();
  assert.equal(chosen.length, 0);
  ui.close();
});
test("graph initial counts survive its independent protection read; dossiers load only after selection", async () => {
  const calls = [],
    api = async (_, { operation, args }) => {
      calls.push(operation);
      if (operation === "project_protection")
        return {
          fingerprint: "a".repeat(64),
          protectedPaths: ["src/main/java/Protected.java"],
        };
      if (operation === "project_units")
        return {
          id: "project-1",
          revision: "revision-1",
          workspaceRevision: "ws",
          counts: { items: 14 },
          units:
            args.kind === "items"
              ? [{ id: "unit-1", name: "Blade", fileCount: 1 }]
              : [],
          filteredCount: 1,
          nextOffset: null,
        };
      if (operation === "project_unit_read")
        return {
          id: "project-1",
          revision: "revision-1",
          unit: {
            id: "unit-1",
            name: "Blade",
            kind: "items",
            notes: "Complex behavior",
            files: ["src/main/java/Protected.java"],
          },
        };
      throw new Error(operation);
    };
  const p = project(),
    ui = harness("MakerGraph", {
      api,
      native: true,
      project: p,
      point: p.checkpoints[0],
      activity: [],
      running: false,
      changed: [],
      onScope() {},
      onSource() {},
      onEdit() {},
    });
  await ui.flush();
  assert.ok(content(ui.tree).includes("物品14"));
  assert.ok(!calls.includes("project_unit_read"));
  const domain = elements(
    ui.tree,
    (n) =>
      n.props?.className?.includes?.("ma-domain") &&
      content(n).includes("物品"),
  )[0];
  domain.props.onClick();
  await ui.flush();
  elements(
    ui.tree,
    (n) => n.props?.className?.startsWith?.("ma-unit") && n.type === "button",
  )[0].props.onClick();
  await ui.flush();
  assert.ok(content(ui.tree).includes("Complex behavior"));
  assert.ok(
    elements(
      ui.tree,
      (n) => n.props["aria-label"] === ui.i18n.t("agent.protected"),
    ).length > 0,
  );
  ui.close();
});
test("charter CAS preserves edited drafts while a save or project refresh is late", async () => {
  const slow = deferred(),
    calls = [],
    draft = harness("ExperimentalProjects", {
      api: async () => ({}),
      native: false,
    }).props.draft.current;
  let saved = 0;
  const p = project();
  const ui = harness("MakerCharter", {
    api: async (_, payload) => {
      calls.push(payload);
      return slow.promise;
    },
    native: true,
    project: p,
    draft,
    onSaved: async () => {
      saved++;
    },
  });
  await ui.flush();
  const textarea = () => elements(ui.tree, (n) => n.type === "textarea")[0];
  textarea().props.onChange({ target: { value: "First draft" } });
  await ui.flush();
  await click(ui, "agent.save");
  textarea().props.onChange({ target: { value: "Newer draft" } });
  await ui.flush();
  slow.resolve({ ...p, revision: "revision-2" });
  await ui.flush();
  assert.equal(saved, 0);
  assert.equal(draft.notes.get(p.id).notes, "Newer draft");
  ui.replace({
    project: { ...p, revision: "revision-2", notes: "Remote charter" },
  });
  await ui.flush();
  assert.equal(textarea().props.value, "Newer draft");
  assert.equal(button(ui, "agent.save").props.disabled, true);
  await click(ui, "agent.adoptRevision");
  assert.equal(draft.notes.get(p.id).revision, "revision-2");
  assert.equal(button(ui, "agent.save").props.disabled, false);
  assert.equal(calls[0].args.expectedRevision, "revision-1");
  ui.close();
});
test("long threads reveal six rounds at a time and keep the actual newest round active", async () => {
  const long = {
    ...thread,
    rounds: Array.from({ length: 25 }, (_, i) => ({
      ...round,
      jobId: `job-${i}`,
    })),
  };
  const ui = harness("MakerThreadView", {
    thread: long,
    api: async () => ({}),
    native: true,
    units: [],
    recordedSources: [],
    onDiff() {},
    onFollowup() {},
    onCommitted: async () => {},
    onPending() {},
    onActivity() {},
    onSource() {},
  });
  await ui.flush();
  let rows = elements(ui.tree, (n) => n.type?.name === "MakerRoundView");
  assert.equal(rows.length, 6);
  assert.equal(rows.at(-1).props.round.jobId, "job-24");
  assert.equal(rows.at(-1).props.latest, true);
  assert.ok(rows.slice(0, -1).every((row) => !row.props.latest));
  elements(ui.tree, (n) => n.type === "button")[0].props.onClick();
  await ui.flush();
  assert.equal(
    elements(ui.tree, (n) => n.type?.name === "MakerRoundView").length,
    12,
  );
  ui.close();
});
