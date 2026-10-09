import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import { MakerThreads, makerPlanTool } from "../vendor/maker/threads.mjs";
import { MakerProjects } from "../vendor/maker/projects.mjs";
import { AgentRuntime, createDefaultTools } from "../src/index.mjs";
const scratch = fileURLToPath(
  new URL("../../../work/maker-agent/tests/", import.meta.url),
);
async function fixture(t) {
  await fs.mkdir(scratch, { recursive: true });
  const root = await fs.mkdtemp(path.join(scratch, "threads-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const jobs = new Map(),
    reviews = new Map(),
    calls = [];
  const projects = await new MakerProjects(root, async (jobId, operationId) => {
    const job = jobs.get(jobId);
    assert.ok(job.operations.some((op) => op.id === operationId));
    return { job, snapshot: { fingerprint: "a".repeat(64) } };
  }).init();
  const deps = {
    projects,
    job: async (id) => {
      assert.ok(jobs.has(id));
      return jobs.get(id);
    },
    create: async (args) => {
      const id = randomUUID();
      jobs.set(id, {
        id,
        profile: "maker",
        status: "completed",
        input: { makerThread: args.makerThread },
        operations: [],
        reviewIds: [],
        result: { text: "Recorded response" },
      });
      calls.push("create");
      return { jobId: id };
    },
    start: async (id) => {
      assert.ok(
        (await threads.list()).threads.some((thread) =>
          thread.rounds.some((round) => round.jobId === id),
        ),
      );
      calls.push("start");
    },
    abort: async () => calls.push("abort"),
    review: async (_, id) => reviews.get(id),
    apply: async (jobId, id) => {
      calls.push("apply");
      reviews.get(id).status = "applied";
      jobs
        .get(jobId)
        .operations.push({
          id: `host-${id}`,
          status: "completed",
          result: { outputDirectory: "reviewed/copy" },
        });
    },
  };
  const threads = await new MakerThreads(root, deps).init();
  const run = (args = {}) =>
    threads.run({
      name: "Example mod",
      notes: "Preserve game balance",
      prompt: "Develop complex rules",
      mode: "execute",
      ...args,
    });
  return { root, jobs, reviews, calls, projects, threads, run, deps };
}
test("thread admission persists before starting; restart restores modes, scope and rounds", async (t) => {
  const f = await fixture(t),
    thread = await f.run({
      mode: "plan",
      scopes: [
        {
          id: randomUUID(),
          name: "Rules",
          kind: "rules",
          notes: "Balance",
          files: ["src/main/java/Rules.java"],
        },
      ],
    });
  assert.deepEqual(f.calls, ["create", "start"]);
  const reopened = await new MakerThreads(f.root, f.deps).init();
  assert.deepEqual(await reopened.read(thread.id), thread);
  const next = await reopened.run({
    threadId: thread.id,
    prompt: "Execute the plan",
    mode: "execute",
  });
  assert.equal(next.rounds.length, 2);
  assert.equal(
    f.jobs.get(next.rounds[1].jobId).input.makerThread.recent[0].response,
    "Recorded response",
  );
});
test("failed metadata admission cancels the unstarted shared job", async (t) => {
  const f = await fixture(t);
  f.threads.save = async () => {
    throw new Error("disk failure");
  };
  await assert.rejects(f.run(), /disk failure/);
  assert.deepEqual(f.calls, ["create", "abort"]);
  assert.equal((await f.threads.list()).threads.length, 0);
});
test("pending review blocks another round; a thread cannot be moved to another project", async (t) => {
  const f = await fixture(t),
    thread = await f.run(),
    job = f.jobs.get(thread.rounds[0].jobId),
    reviewId = randomUUID();
  job.reviewIds = [reviewId];
  f.reviews.set(reviewId, { status: "pending" });
  await assert.rejects(f.run({ threadId: thread.id }), /Review or return/);
  assert.equal(f.calls.filter((c) => c === "create").length, 1);
  job.reviewIds = [];
  job.operations = [
    {
      id: "generated",
      status: "completed",
      result: { outputDirectory: "generated" },
    },
  ];
  const project = await f.projects.create({
    name: "Other",
    jobId: job.id,
    operationId: "generated",
    label: "v1",
  });
  await assert.rejects(
    f.run({
      threadId: thread.id,
      id: project.id,
      expectedRevision: project.revision,
      checkpointId: project.checkpoints[0].id,
    }),
    /another project/,
  );
});
test("approval archives once, and a lost thread-link write recovers without replaying the patch", async (t) => {
  const f = await fixture(t),
    thread = await f.run(),
    jobId = thread.rounds[0].jobId,
    reviewId = randomUUID(),
    digest = "b".repeat(64);
  f.reviews.set(reviewId, { reviewDigest: digest, status: "pending" });
  const save = f.threads.save.bind(f.threads);
  let fail = true;
  f.threads.save = async (value) => {
    if (fail) {
      fail = false;
      throw new Error("metadata lost");
    }
    return save(value);
  };
  await assert.rejects(
    f.threads.approve({ threadId: thread.id, jobId, reviewId, digest }),
    /metadata lost/,
  );
  assert.equal((await f.projects.list()).length, 1);
  const recovered = await f.threads.approve({
    threadId: thread.id,
    jobId,
    reviewId,
    digest,
  });
  assert.equal(recovered.project.checkpoints.length, 1);
  assert.equal(f.calls.filter((c) => c === "apply").length, 1);
  assert.equal(
    (await f.threads.read(thread.id)).projectId,
    recovered.project.id,
  );
  await f.threads.approve({ threadId: thread.id, jobId, reviewId, digest });
  assert.equal((await f.projects.list())[0].checkpoints.length, 1);
});
test("wrong digest, unrelated source and active job are rejected before mutation", async (t) => {
  const f = await fixture(t),
    thread = await f.run(),
    jobId = thread.rounds[0].jobId,
    reviewId = randomUUID();
  f.reviews.set(reviewId, { reviewDigest: "b".repeat(64), status: "pending" });
  await assert.rejects(
    f.threads.approve({
      threadId: thread.id,
      jobId,
      reviewId,
      digest: "c".repeat(64),
    }),
    /Review changed/,
  );
  await assert.rejects(
    f.threads.archive({
      threadId: thread.id,
      jobId: randomUUID(),
      operationId: "other",
    }),
    /does not belong/,
  );
  f.jobs.get(jobId).status = "running";
  await assert.rejects(
    f.threads.approve({
      threadId: thread.id,
      jobId,
      reviewId,
      digest: "b".repeat(64),
    }),
    /Wait/,
  );
  assert.ok(!f.calls.includes("apply"));
});
test("corrupt and future thread records are retained and cannot be treated as empty", async (t) => {
  const f = await fixture(t),
    thread = await f.run(),
    file = path.join(f.root, "maker-threads", `${thread.id}.json`);
  const corrupt = JSON.stringify({ ...thread, schemaVersion: 9 });
  await fs.writeFile(file, corrupt);
  assert.equal((await f.threads.list()).errors.length, 1);
  await assert.rejects(f.run({ threadId: thread.id }), /Unsupported/);
  assert.equal(await fs.readFile(file, "utf8"), corrupt);
});
test("a source archive failure after applying can retry without another approval", async (t) => {
  const f = await fixture(t),
    thread = await f.run(),
    jobId = thread.rounds[0].jobId,
    reviewId = randomUUID(),
    digest = "b".repeat(64);
  f.reviews.set(reviewId, { reviewDigest: digest, status: "pending" });
  const create = f.projects.create.bind(f.projects);
  f.projects.create = async () => {
    throw new Error("version write failed");
  };
  await assert.rejects(
    f.threads.approve({ threadId: thread.id, jobId, reviewId, digest }),
    /version write/,
  );
  f.projects.create = create;
  await f.threads.approve({ threadId: thread.id, jobId, reviewId, digest });
  assert.equal(f.calls.filter((c) => c === "apply").length, 1);
});
test("ask and plan are enforced as read-only capabilities; execute requires a plan receipt", async (t) => {
  const f = await fixture(t);
  const observations = [];
  const provider = {
    id: "offline",
    run: async (ctx) => {
      const mode = ctx.job.input.makerThread.mode;
      observations.push({
        mode,
        tools: ctx.toolDescriptions.map((t) => t.name),
      });
      try {
        await ctx.invoke("write", "workspace.write", {
          path: "scratch/example.txt",
          content: "forbidden",
        });
        observations.at(-1).write = true;
      } catch (e) {
        observations.at(-1).error = e.code;
      }
      if (mode === "execute") {
        await assert.rejects(ctx.invoke("before", "maker.generate", {}), {
          code: "PLAN_REQUIRED",
        });
        await ctx.invoke("plan", "maker.report_plan", {
          steps: ["Read the selected unit", "Propose changes for review"],
        });
        await ctx.invoke("after", "maker.generate", {});
      }
      return { text: "Offline capability check" };
    },
  };
  const tools = createDefaultTools()
    .register(makerPlanTool)
    .register({
      name: "maker.generate",
      description: "Isolated receipt only",
      replaySafety: "idempotent",
      inputSchema: { type: "object" },
      validate: () => {},
      execute: async () => ({ status: "fixture" }),
    });
  const runtime = await new AgentRuntime({
    root: path.join(f.root, "jobs"),
    tools,
    providers: [provider],
  }).init();
  for (const mode of ["ask", "plan", "execute"]) {
    const job = await runtime.createJob({
      profile: "maker",
      provider: "offline",
      input: { makerThread: { id: randomUUID(), mode } },
    });
    const done = await runtime.start(job.id);
    assert.equal(done.status, "completed");
  }
  for (const record of observations.slice(0, 2)) {
    assert.equal(record.error, "TOOL_DENIED");
    assert.ok(!record.tools.includes("workspace.write"));
    assert.ok(!record.tools.includes("maker.generate"));
  }
  assert.equal(observations[2].write, true);
});
test("captured handwritten locks block Agent edits and continuation cannot regenerate a fresh project", async (t) => {
  const f = await fixture(t),
    attempts = [];
  const provider = {
    id: "offline-locks",
    run: async (ctx) => {
      await ctx.invoke("plan", "maker.report_plan", {
        steps: ["Read source and preserve handwritten files"],
      });
      await assert.rejects(ctx.invoke("replace", "maker.generate", {}), {
        code: "TOOL_DENIED",
      });
      await assert.rejects(
        ctx.invoke("locked", "maker.preview_source_edit", {
          request: { edits: [{ path: "src/main/java/Protected.java" }] },
        }),
        { code: "HANDWRITTEN_PROTECTED" },
      );
      await ctx.invoke("allowed", "maker.preview_source_edit", {
        request: { edits: [{ path: "src/main/java/NewFeature.java" }] },
      });
      return { text: "Protected file preserved" };
    },
  };
  const tools = createDefaultTools().register(makerPlanTool);
  for (const name of ["maker.generate", "maker.preview_source_edit"])
    tools.register({
      name,
      description: "Offline stub",
      replaySafety: "idempotent",
      inputSchema: { type: "object" },
      validate: () => {},
      execute: async (args) => {
        attempts.push(args);
        return { status: "fixture" };
      },
    });
  const runtime = await new AgentRuntime({
    root: path.join(f.root, "locks"),
    tools,
    providers: [provider],
  }).init();
  const job = await runtime.createJob({
    profile: "maker",
    provider: provider.id,
    input: {
      sourceOperationId: "seed",
      makerThread: {
        id: randomUUID(),
        mode: "execute",
        protectedPaths: ["src/main/java/Protected.java"],
      },
    },
  });
  assert.equal((await runtime.start(job.id)).status, "completed");
  assert.equal(attempts.length, 1);
  assert.equal(
    attempts[0].request.edits[0].path,
    "src/main/java/NewFeature.java",
  );
});
test("ownership projection rejects mismatched hashes instead of inventing protection", async () => {
  const { snapshotProtection } = await import("../vendor/maker/protection.mjs");
  const { createHash } = await import("node:crypto");
  const source = Buffer.from("handwritten");
  const meta = {
    schema_version: 1,
    files: {
      "src/main/java/Protected.java": {
        owner: "locked",
        sha256: createHash("sha256").update(source).digest("hex"),
      },
    },
  };
  const snapshot = {
    files: {
      "src/main/java/Protected.java": source,
      "creator-ownership.json": Buffer.from(JSON.stringify(meta)),
    },
  };
  assert.deepEqual(snapshotProtection(snapshot), [
    "src/main/java/Protected.java",
  ]);
  meta.files["src/main/java/Protected.java"].sha256 = "0".repeat(64);
  snapshot.files["creator-ownership.json"] = Buffer.from(JSON.stringify(meta));
  assert.throws(() => snapshotProtection(snapshot), /differs/);
});
