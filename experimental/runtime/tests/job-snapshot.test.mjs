import test from "node:test";
import assert from "node:assert/strict";
import { readJobSnapshot } from "../../job-snapshot.mjs";

const id = "00000000-0000-0000-0000-000000000000";
const running = { id, profile: "harness", status: "running", revision: 1,
  updatedAt: "2026-01-01T00:00:00.000Z", operations: [], result: null };

test("completion during a read cannot pair its new status with an older empty result", async () => {
  let current = structuredClone(running), reads = 0;
  const runtime = {
    async getJob() {
      reads++;
      const captured = structuredClone(current);
      current = { ...current, status: "completed", revision: 2, result: { text: "Complete reply" } };
      return captured;
    },
    async getPublicJobSummary() { assert.fail("Do not read another generation for the summary"); },
  };
  const first = await readJobSnapshot(runtime, id, () => true);
  assert.equal(first.summary.status, "running");
  assert.equal(first.summary.revision, 1);
  assert.equal(first.job.result, null);
  assert.equal(reads, 1);
  const next = await readJobSnapshot(runtime, id, () => true);
  assert.equal(next.summary.status, "completed");
  assert.equal(next.summary.revision, 2);
  assert.equal(next.job.result.text, "Complete reply");
});

test("orphan active states stay interrupted and a foreign task is rejected", async () => {
  const captured = await readJobSnapshot({ getJob: async () => structuredClone(running) }, id, () => false);
  assert.equal(captured.summary.status, "interrupted");
  assert.equal(captured.job.status, "running", "A public projection cannot mutate the stored job");
  await assert.rejects(readJobSnapshot({ getJob: async () => ({ ...running, id: "11111111-1111-1111-1111-111111111111" }) }, id, () => true),
    { code: "JOB_IDENTITY_CHANGED" });
});
