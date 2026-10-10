import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { agentContext } from "../agent-context.mjs";
import { prepareCodeJob } from "../agent-adapter.mjs";
const source = (path, text) => ({
  path,
  data: Buffer.from(text).toString("base64"),
  sha256: createHash("sha256").update(text).digest("hex"),
});
const input = {
  project: { id: "p", name: "A mod" },
  conversation: { id: "c", messages: [] },
  request: "Make a component",
  mode: "plan",
  target: { minecraft: "1.21.1", loaders: ["Fabric", "NeoForge"] },
  sourceFiles: [],
};
function host() {
  const writes = [],
    saved = [],
    cancelled = [],
    admitted = [];
  return {
    writes,
    saved,
    cancelled,
    admitted,
    preset: { selection: {}, limits: {} },
    provider: "fixture",
    jobs: [],
    runtime: {
      createJob: async (args) => ({
        id: "fixture-job",
        operations: [],
        input: args.input,
      }),
      store: {
        workspace: async () => ({
          writeBytes: async (path, bytes) => {
            writes.push([path, bytes]);
            return { path };
          },
        }),
        save: async (job) => saved.push(job),
      },
      cancel: async (id) => cancelled.push(id),
    },
    admit: async (job, preset, deferred) => {
      admitted.push({ job, preset, deferred });
      return { jobId: job.id };
    },
  };
}
test("context projection never mutates durable history and distinguishes omitted messages", () => {
  const messages = Array.from({ length: 20 }, (_, i) => ({
    role: i % 2 ? "assistant" : "user",
    content: "message " + i,
  }));
  const before = JSON.stringify(messages);
  const ctx = agentContext({
    ...input,
    conversation: { id: "c", messages },
    source: { "Main.java": "// data" },
  });
  assert.equal(ctx.projection.omitted, 12);
  assert.equal(ctx.thread.recent.length, 8);
  assert.match(ctx.thread.charter, /quoted data/);
  assert.equal(JSON.stringify(messages), before);
  assert.equal(
    agentContext({
      ...input,
      conversation: {
        id: "c",
        messages: messages.slice(-8),
        omittedMessages: 12,
      },
    }).projection.omitted,
    12,
  );
});
test("preparation is deferred until native conversation ownership is saved", async () => {
  const h = host();
  const result = await prepareCodeJob(input, h);
  assert.equal(result.jobId, "fixture-job");
  assert.equal(h.admitted[0].deferred, true);
  assert.equal(h.writes.length, 0);
  assert.equal(h.admitted[0].job.input.spec.target, "fabric-1.21.1");
  assert.equal(h.admitted[0].job.input.spec.items.length, 1);
});
test("existing source is hashed and staged in job copy without ambient project paths", async () => {
  const h = host();
  await prepareCodeJob(
    {
      ...input,
      sourceFiles: [
        source("creator-spec.json", '{"target":"fabric-1.21.1"}'),
        source("src/Main.java", "handwritten"),
      ],
      source: { "src/Main.java": "handwritten" },
    },
    h,
  );
  assert.equal(h.writes.length, 2);
  assert.equal(h.saved.length, 1);
  assert.equal(h.saved[0].operations[0].result.originalSourceModified, false);
  assert.equal(
    h.saved[0].input.sourceInventory[1].expected_sha256,
    source("x", "handwritten").sha256,
  );
});
test("unknown authoring target and foreign source refuse writes before admission", async () => {
  for (const a of [
    {
      ...input,
      mode: "execute",
      target: { minecraft: "1.21.4", loaders: ["Fabric"] },
    },
    {
      ...input,
      mode: "execute",
      sourceFiles: [source("src/Main.java", "foreign source")],
    },
  ]) {
    const h = host();
    await assert.rejects(prepareCodeJob(a, h), /adapter|supports/);
    assert.equal(h.admitted.length, 0);
  }
});
test("corrupt native source and traversal never reach workspace writes", async () => {
  for (const entry of [
    source("../x", "bad"),
    {
      ...source("src/A.java", "good"),
      data: Buffer.from("changed").toString("base64"),
    },
  ]) {
    const h = host();
    await assert.rejects(
      prepareCodeJob({ ...input, sourceFiles: [entry] }, h),
      /snapshot/,
    );
    assert.equal(h.writes.length, 0);
  }
});
test("staging failure cancels the prepared task and does not admit it", async () => {
  const h = host();
  h.runtime.store.workspace = async () => ({
    writeBytes: async () => {
      throw new Error("disk full");
    },
  });
  await assert.rejects(
    prepareCodeJob(
      { ...input, sourceFiles: [source("creator-spec.json", "{}")] },
      h,
    ),
    /disk full/,
  );
  assert.deepEqual(h.cancelled, ["fixture-job"]);
  assert.equal(h.admitted.length, 0);
});
