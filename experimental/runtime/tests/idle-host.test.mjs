/** Private retirement probes; fixtures stay under work and use loopback only. */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import http from "node:http";
import readline from "node:readline";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { prepareIdle } from "../../idle-state.mjs";
import { SubprocessSupervisor } from "../src/subprocesses.mjs";
const hostPath = fileURLToPath(new URL("../../host.mjs", import.meta.url));
const scratch = fileURLToPath(
  new URL("../../../work/rh-code-hibernation/node-fixtures/", import.meta.url),
);
async function fixture(t) {
  await fs.mkdir(scratch, { recursive: true });
  const root = await fs.mkdtemp(path.join(scratch, "idle-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  return root;
}
async function until(check) {
  const end = Date.now() + 7000;
  while (Date.now() < end) {
    const value = await check();
    if (value) return value;
    await new Promise((r) => setTimeout(r, 5));
  }
  throw new Error("Isolated idle check timed out");
}
function probe(root) {
  return {
    runtime: {
      store: { root, directory: (id) => path.join(root, id) },
      hasActiveWork: () => false,
    },
    reviews: { getReview: async () => ({ status: "cancelled" }) },
    children: [],
    pendingJobs: new Set(),
    liveRuns: new Map(),
    pendingExtensions: new Map(),
    sourceImports: new Map(),
    safeMode: false,
    selection: null,
    preset: null,
  };
}
test("every volatile owner blocks retirement; durable selection needs only its preset ID", async (t) => {
  const root = await fixture(t);
  const base = probe(root);
  assert.deepEqual(await prepareIdle(base), {
    busy: false,
    resumePresetId: null,
  });
  for (const key of [
    "pendingJobs",
    "liveRuns",
    "pendingExtensions",
    "sourceImports",
  ]) {
    const state = probe(root);
    state[key].add?.("owned");
    state[key].set?.("owned", {});
    assert.deepEqual(await prepareIdle(state), { busy: true }, key);
  }
  assert.equal((await prepareIdle({ ...base, safeMode: true })).busy, true);
  assert.equal(
    (
      await prepareIdle({
        ...base,
        runtime: { ...base.runtime, hasActiveWork: () => true },
      })
    ).busy,
    true,
  );
  assert.equal(
    (await prepareIdle({ ...base, children: [{ hasActiveWork: () => true }] }))
      .busy,
    true,
  );
  const selection = { key: "private-fixture", model: "offline" };
  assert.equal((await prepareIdle({ ...base, selection })).busy, true);
  assert.equal(
    (
      await prepareIdle({
        ...base,
        selection,
        preset: { id: "saved", selection: { ...selection, model: "edited" } },
      })
    ).busy,
    true,
  );
  const ready = await prepareIdle({
    ...base,
    selection,
    preset: { id: "saved", selection: { ...selection } },
  });
  assert.deepEqual(ready, { busy: false, resumePresetId: "saved" });
  assert.equal(JSON.stringify(ready).includes(selection.key), false);
});
test("legacy review beyond display history and unreadable state pin retirement", async (t) => {
  const root = await fixture(t);
  for (let n = 0; n < 101; n++) {
    const id = `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
    await fs.mkdir(path.join(root, id, "review-control/reviews"), {
      recursive: true,
    });
    await fs.writeFile(
      path.join(root, id, "review-control/reviews/review.json"),
      "{}",
    );
  }
  const state = probe(root);
  let reads = 0;
  state.reviews.getReview = async () => ({
    status: ++reads === 101 ? "pending" : "cancelled",
  });
  assert.equal((await prepareIdle(state)).busy, true);
  assert.equal(reads, 101);
  state.reviews.getReview = async () => {
    throw new Error("invalid persisted review");
  };
  assert.equal((await prepareIdle(state)).busy, true);
});
test("silent supervised child pins until it finishes without cancellation", async (t) => {
  const root = await fixture(t);
  const supervisor = new SubprocessSupervisor({
    trustedCommands: {
      wait: {
        executable: process.execPath,
        args: ["-e", 'setTimeout(() => process.stdout.write("done"), 150)'],
      },
    },
  });
  t.after(() => supervisor.shutdown());
  const run = supervisor.run("wait", { cwd: root });
  assert.equal(
    (await prepareIdle({ ...probe(root), children: [supervisor] })).busy,
    true,
  );
  const result = await run;
  assert.equal(result.status, "completed");
  assert.equal(result.stdout, "done");
  assert.equal(
    (await prepareIdle({ ...probe(root), children: [supervisor] })).busy,
    false,
  );
});
test(
  "stdio probe preserves silent AI and deferred jobs; restart restores durable provider choice",
  { timeout: 20000 },
  async (t) => {
    const root = await fixture(t);
    let requests = 0;
    const server = http.createServer(async (req) => {
      for await (const _ of req) {
      }
      requests++;
    });
    await new Promise((r) => server.listen(0, "127.0.0.1", r));
    let child, lines, replies;
    function open() {
      child = spawn(process.execPath, [hostPath, root], {
        stdio: ["pipe", "pipe", "pipe"],
      });
      child.stderr.resume();
      replies = [];
      lines = readline.createInterface({ input: child.stdout });
      lines.on("line", (line) => replies.push(JSON.parse(line)));
    }
    async function rpc(operation, args = {}) {
      child.stdin.write(JSON.stringify({ operation, args }) + "\n");
      const reply = await until(() => replies.shift());
      assert.equal(reply.ok, true, reply.error);
      return reply.result;
    }
    t.after(async () => {
      child?.kill("SIGKILL");
      lines?.close();
      server.closeAllConnections();
      await new Promise((r) => server.close(r));
    });
    open();
    const key = "loopback-only-fixture-key";
    const saved = await rpc("provider_preset_save", {
      name: "Offline",
      config: {
        provider: "pcl-custom",
        model: "offline",
        api: "openai-completions",
        baseUrl: `http://127.0.0.1:${server.address().port}/v1`,
        key,
      },
    });
    await rpc("provider_start");
    const running = await rpc("job_create", {
      profile: "maker",
      mode: "live",
      spec: {},
      prompt: "Wait silently",
    });
    await until(() => requests === 1);
    assert.deepEqual(await rpc("idle_prepare"), { busy: true });
    assert.equal(
      (await rpc("job_read", { jobId: running.jobId })).summary.status,
      "running",
    );
    await rpc("job_cancel", { jobId: running.jobId });
    const deferred = await rpc("rh_code_run", {
      project: { id: "fixture", name: "Fixture" },
      conversation: { id: "conversation", messages: [] },
      request: "Offline pending admission",
      mode: "ask",
      target: { minecraft: "1.21.1", loaders: ["Fabric"] },
      source: {},
      sourceFiles: [],
    });
    assert.deepEqual(await rpc("idle_prepare"), { busy: true });
    assert.equal(requests, 1, "deferred admission must not contact provider");
    await rpc("job_cancel", { jobId: deferred.jobId });
    const ready = await until(async () => {
      const result = await rpc("idle_prepare");
      return !result.busy && result;
    });
    assert.deepEqual(ready, { busy: false, resumePresetId: saved.selectedId });
    assert.equal(JSON.stringify(ready).includes(key), false);
    const exited = new Promise((r) => child.once("exit", r));
    await rpc("shutdown");
    child.kill("SIGTERM");
    await exited;
    lines.close();
    open();
    await rpc("idle_resume", { presetId: ready.resumePresetId });
    const resumed = await rpc("status");
    assert.equal(resumed.liveConfigured, true);
    assert.equal(resumed.aiPresets.selectedId, saved.selectedId);
    assert.equal(JSON.stringify(resumed).includes(key), false);
    assert.equal(
      (await rpc("job_read", { jobId: deferred.jobId })).summary.status,
      "cancelled",
    );
    const exitedAgain = new Promise((r) => child.once("exit", r));
    await rpc("shutdown");
    child.kill("SIGTERM");
    await exitedAgain;
  },
);
