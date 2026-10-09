import assert from "node:assert/strict";
import test from "node:test";
import {
  harness,
  elements,
  content,
  button,
  click,
  deferred,
  project,
  checkpoint,
} from "./maker-test-harness.mjs";
function dialog(ui) {
  return elements(
    ui.tree,
    (node) => node.type?.name === "InstanceOperationDialog",
  )[0];
}
function reviewFixture() {
  const calls = [],
    intercept = new Map();
  let status = "pending";
  const value = () => ({
    jobId: "job-1",
    reviewId: "review-1",
    reviewDigest: "a".repeat(64),
    status,
    kind: "maker",
    preview: { diffs: [{ path: "src/main/java/A.java", diff: "-old\n+new" }] },
  });
  const api = async (_, { operation, args }) => {
    calls.push({ operation, args });
    if (intercept.has(operation)) return intercept.get(operation)(args);
    if (operation === "review_read") return value();
    if (operation === "review_apply") {
      status = "applied";
      return { reviewId: "review-1" };
    }
    if (operation === "review_cancel")
      return { reviewId: "review-1", status: "cancelled" };
    throw new Error(operation);
  };
  return {
    api,
    calls,
    intercept,
    value,
    status(next) {
      status = next;
    },
  };
}
async function review(f = reviewFixture(), extra = {}) {
  const applied = [];
  const ui = harness("MakerReviewDialog", {
    api: f.api,
    native: true,
    jobId: "job-1",
    reviewId: "review-1",
    onClose() {},
    onApplied: async (id) => {
      applied.push(id);
    },
    ...extra,
  });
  await ui.flush();
  return { ui, f, applied };
}
test("review binds job, identity and digest before confirmation", async () => {
  for (const wrong of [
    { jobId: "other" },
    { reviewId: "other" },
    { reviewDigest: "b".repeat(64) },
  ]) {
    const f = reviewFixture();
    f.intercept.set("review_read", () => ({ ...f.value(), ...wrong }));
    const { ui } = await review(f, { digest: "a".repeat(64) });
    assert.equal(dialog(ui).props.confirmDisabled, true);
    assert.match(content(ui.tree), /返回结果/);
  }
});
test("duplicate queued approval is admitted once and calls the owning continuation", async () => {
  const f = reviewFixture(),
    pending = deferred();
  f.intercept.set("review_apply", () => pending.promise);
  const { ui, applied } = await review(f);
  const confirm = dialog(ui).props.onConfirm;
  confirm();
  confirm();
  await ui.flush();
  assert.equal(
    f.calls.filter((value) => value.operation === "review_apply").length,
    1,
  );
  pending.resolve({ reviewId: "review-1" });
  await ui.flush();
  assert.deepEqual(applied, ["host-review-1"]);
  assert.equal(dialog(ui).props.confirmDisabled, true);
});
test("uncertain apply acknowledgement remains blocked until authoritative pending status", async () => {
  const f = reviewFixture();
  f.intercept.set("review_apply", () => {
    throw new Error("acknowledgement uncertain");
  });
  const { ui } = await review(f);
  dialog(ui).props.onConfirm();
  await ui.flush();
  assert.equal(dialog(ui).props.confirmDisabled, true);
  dialog(ui).props.onConfirm();
  await ui.flush();
  assert.equal(
    f.calls.filter((value) => value.operation === "review_apply").length,
    1,
  );
  f.status("applied");
  await click(ui, "maker.refreshReview");
  assert.equal(dialog(ui).props.confirmDisabled, true);
  f.status("pending");
  await click(ui, "maker.refreshReview");
  assert.equal(dialog(ui).props.confirmDisabled, false);
});
test("an acknowledged copy is never replayed after its display callback fails", async () => {
  const { ui, f } = await review(undefined, {
    onApplied: async () => {
      throw new Error("refresh failed");
    },
  });
  dialog(ui).props.onConfirm();
  await ui.flush();
  assert.match(content(ui.tree), /refresh failed/);
  dialog(ui).props.onConfirm();
  await ui.flush();
  assert.equal(
    f.calls.filter((value) => value.operation === "review_apply").length,
    1,
  );
});
test("admitted old-API apply may finish in host but cannot update a replacement dialog", async () => {
  const f = reviewFixture(),
    pending = deferred();
  f.intercept.set("review_apply", () => pending.promise);
  const { ui, applied } = await review(f);
  dialog(ui).props.onConfirm();
  const replacement = reviewFixture();
  ui.replace({ api: replacement.api });
  await ui.flush();
  pending.resolve({ reviewId: "review-1" });
  await ui.flush();
  assert.deepEqual(applied, []);
  assert.equal(dialog(ui).props.confirmDisabled, false);
});
test("unmounted review events cannot confirm or cancel; delayed read never opens old content", async () => {
  const { ui, f } = await review();
  const events = dialog(ui).props;
  ui.close();
  events.onConfirm();
  events.onClose();
  await ui.flush();
  assert.equal(
    f.calls.filter((value) => value.operation !== "review_read").length,
    0,
  );
  const old = reviewFixture(),
    pending = deferred();
  old.intercept.set("review_read", () => pending.promise);
  const next = harness("MakerReviewDialog", {
    api: old.api,
    native: true,
    jobId: "job-1",
    reviewId: "review-1",
    onClose() {},
    onApplied: async () => {},
  });
  const replacement = reviewFixture();
  next.replace({ api: replacement.api });
  await next.flush();
  pending.resolve({
    ...old.value(),
    preview: { diffs: [{ path: "retired.java", diff: "retired" }] },
  });
  await next.flush();
  assert.ok(!content(next.tree).includes("retired.java"));
});
function sourceFixture() {
  const calls = [],
    intercept = new Map(),
    paths = ["src/main/java/A.java", "src/main/java/B.java"];
  const api = async (_, { operation, args }) => {
    calls.push({ operation, args });
    if (intercept.has(operation)) return intercept.get(operation)(args);
    if (operation === "project_files")
      return {
        fingerprint: checkpoint.fingerprint,
        fileCount: 2,
        editableSnapshot: true,
        files: paths.map((path) => ({ path, sha256: path, text: true })),
        nextOffset: null,
      };
    if (operation === "project_file_read")
      return {
        path: args.path,
        sha256: args.path,
        offset: 0,
        content: `class ${args.path.includes("A.java") ? "A" : "B"} {}`,
        nextOffset: null,
      };
    if (operation === "artifact_read")
      return {
        path: args.path,
        sha256: args.path,
        content: "class A {}",
        binary: false,
      };
    if (operation === "maker_review")
      return { reviewId: "review-1", reviewDigest: "a".repeat(64) };
    throw new Error(operation);
  };
  return { api, calls, intercept, paths };
}
async function source(f = sourceFixture()) {
  const seed = harness("ExperimentalProjects", {
    api: async () => {},
    native: false,
  }).state.createProjectManagerDraft();
  const ui = harness("MakerSource", {
    api: f.api,
    native: true,
    project: project(),
    point: checkpoint,
    source: checkpoint,
    draft: seed,
  });
  await ui.flush();
  return { ui, f, seed };
}
async function choose(ui, file) {
  const tree = elements(
    ui.tree,
    (node) => node.type?.name === "MakerFileTree",
  )[0];
  assert.ok(tree);
  const path = tree.props.names.find((name) => name.endsWith(file));
  assert.ok(path);
  tree.props.onChoose(path);
  await ui.flush();
}

test("explorer opens receipt-bound readonly chunks; editing explicitly loads the bounded snapshot", async () => {
  const { ui, f, seed } = await source();
  await choose(ui, "A.java");
  assert.equal(
    elements(ui.tree, (node) => node.type === "textarea")[0].props.readOnly,
    true,
  );
  assert.equal(
    f.calls.filter((value) => value.operation === "artifact_read").length,
    0,
  );
  await click(ui, "maker.editFile");
  let editor = elements(ui.tree, (node) => node.type === "textarea")[0];
  assert.equal(editor.props.readOnly, false);
  editor.props.onChange({ target: { value: "class A { /* handwritten */ }" } });
  await ui.flush();
  assert.equal(seed.buffers.size, 1);
  await click(ui, "maker.previewFileChanges");
  assert.equal(
    f.calls.find((value) => value.operation === "maker_review").args.sha256,
    "src/main/java/A.java",
  );
  assert.equal(
    elements(ui.tree, (node) => node.type?.name === "MakerReviewDialog").length,
    1,
  );
});
test("delayed source reads and previews cannot overwrite a newer selected file", async () => {
  const f = sourceFixture(),
    pending = deferred();
  f.intercept.set("project_file_read", (args) =>
    args.path.includes("A.java")
      ? pending.promise
      : {
          path: args.path,
          sha256: args.path,
          offset: 0,
          content: "class B {}",
          nextOffset: null,
        },
  );
  const { ui } = await source(f);
  await choose(ui, "A.java");
  await choose(ui, "B.java");
  pending.resolve({
    path: f.paths[0],
    sha256: f.paths[0],
    content: "RETIRE",
    offset: 0,
    nextOffset: null,
  });
  await ui.flush();
  assert.equal(
    elements(ui.tree, (node) => node.type === "textarea")[0].props.value,
    "class B {}",
  );
});
test("new file selection retires an admitted preview and retains handwritten buffers", async () => {
  const { ui, f, seed } = await source();
  await choose(ui, "A.java");
  await click(ui, "maker.editFile");
  elements(ui.tree, (node) => node.type === "textarea")[0].props.onChange({
    target: { value: "local handwritten" },
  });
  await ui.flush();
  const pending = deferred();
  f.intercept.set("maker_review", () => pending.promise);
  button(ui, "maker.previewFileChanges").props.onClick();
  await choose(ui, "B.java");
  pending.resolve({ reviewId: "old-review" });
  await ui.flush();
  assert.equal(
    elements(ui.tree, (node) => node.type?.name === "MakerReviewDialog").length,
    0,
  );
  assert.equal([...seed.buffers.values()][0].text, "local handwritten");
});
test("Porter source and large registered snapshots stay readonly", async () => {
  const f = sourceFixture();
  f.intercept.set("project_files", () => ({
    fingerprint: checkpoint.fingerprint,
    fileCount: 400,
    editableSnapshot: false,
    files: [{ path: f.paths[0], sha256: f.paths[0], text: true }],
    nextOffset: 50,
  }));
  const { ui } = await source(f);
  await choose(ui, "A.java");
  assert.equal(
    elements(
      ui.tree,
      (node) =>
        node.type === "button" && content(node) === ui.i18n.t("maker.editFile"),
    ).length,
    0,
  );
  ui.replace({ readOnly: true });
  await ui.flush();
  assert.equal(
    elements(ui.tree, (node) => node.type === "textarea")[0].props.readOnly,
    true,
  );
});
test("reopening an edited file retains its buffered draft and validates its source hash", async () => {
  const { ui, f, seed } = await source();
  await choose(ui, "A.java");
  await click(ui, "maker.editFile");
  elements(ui.tree, (node) => node.type === "textarea")[0].props.onChange({
    target: { value: "local draft" },
  });
  await ui.flush();
  await choose(ui, "B.java");
  await choose(ui, "A.java");
  await click(ui, "maker.editFile");
  assert.equal(
    elements(ui.tree, (node) => node.type === "textarea")[0].props.value,
    "local draft",
  );
  await choose(ui, "B.java");
  await choose(ui, "A.java");
  f.intercept.set("artifact_read", (args) => ({
    path: args.path,
    sha256: "changed",
    content: "different",
    binary: false,
  }));
  await click(ui, "maker.editFile");
  assert.ok(content(ui.tree).includes(ui.i18n.t("maker.responseMismatch")));
  assert.equal([...seed.buffers.values()][0].text, "local draft");
});
test("task view distinguishes source, compile and game states without executing builds", async () => {
  const calls = [];
  const api = async (_, { operation }) => {
    calls.push(operation);
    return {
      summary: {
        jobId: "job-1",
        workflow: "maker",
        status: "completed",
        updatedAt: checkpoint.createdAt,
      },
      result: { text: "Complex implementation proposal" },
      reviews: [],
      artifacts: [{ operationId: "generate-1", files: ["A.java"] }],
    };
  };
  const ui = harness("MakerRoundView", {
    api,
    native: true,
    threadId: "thread-1",
    round: { jobId: "job-1", request: "Maintain this mod", mode: "execute" },
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
  assert.ok(content(ui.tree).includes(ui.i18n.t("agent.sourceProduced")));
  assert.ok(content(ui.tree).includes(ui.i18n.t("agent.buildLabel")));
  assert.ok(content(ui.tree).includes(ui.i18n.t("agent.gameLabel")));
  assert.deepEqual(calls, ["job_read"]);
  ui.close();
});

test("source tree groups compact directories and reads only a selected listed path", async () => {
  const selected = [],
    ui = harness("MakerFileTree", {
      names: [
        "src/main/java/A.java",
        "src/main/java/rules/B.java",
        "build.gradle",
      ],
      selected: "",
      disabled: false,
      onChoose: (path) => selected.push(path),
    });
  assert.ok(elements(ui.tree, (node) => node.type === "details").length >= 2);
  assert.ok(content(ui.tree).includes("src/main/java"));
  elements(
    ui.tree,
    (node) => node.type === "button" && content(node) === "B.java",
  )[0].props.onClick();
  assert.deepEqual(selected, ["src/main/java/rules/B.java"]);
});
