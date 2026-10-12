/** Exercise the private native-facing dispatcher, durable JobStore and pinned
 * Pi SDK against a synthetic loopback provider. All project bytes, presets and
 * acknowledgements belong to disposable fixtures; no native apply is invoked.
 */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import http from "node:http";
import readline from "node:readline";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";

const hostPath = fileURLToPath(new URL("../../host.mjs", import.meta.url));
const scratch = fileURLToPath(new URL("../../../work/harness-2026-10-12/host/", import.meta.url));
const projectId = "fixture-project", conversationId = "fixture-conversation";
const finalText = "The fixture proposal is ready for review.";
const privateReasoning = "PRIVATE_REASONING_FIXTURE";
const hash = (text) => createHash("sha256").update(text).digest("hex");
const toolNames = ["list_files", "read_file", "search_files", "report_plan", "propose_changes"];
const exposedTools = [...toolNames, "read_context", "read_history", "search_history"];
const pause = () => new Promise((resolve) => setTimeout(resolve, 15));

async function until(check, label) {
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    const result = await check();
    if (result) return result;
    await pause();
  }
  throw new Error(`Isolated Harness host timed out: ${label}`);
}
async function bounded(promise, milliseconds, label) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_resolve, reject) => {
      timer = setTimeout(() => reject(new Error(`Isolated Harness host timed out: ${label}`)), milliseconds);
    })]);
  } finally { clearTimeout(timer); }
}

// RPC envelopes have no IDs, so permit one outstanding call and fail it when
// the process exits. A timeout retires the process instead of misbinding a late
// response to another request. Teardown always waits for child termination.
class PrivateHost {
  constructor(root) {
    this.child = spawn(process.execPath, [hostPath, root], { stdio: ["pipe", "pipe", "pipe"] });
    this.child.stderr.resume();
    this.exited = new Promise((resolve) => {
      this.child.once("exit", (code, signal) => {
        this.failure = new Error(`Isolated Harness host exited (${code ?? signal})`);
        this.pending?.reject(this.failure);
        resolve({ code, signal });
      });
      this.child.once("error", (error) => {
        this.failure = error;
        this.pending?.reject(error);
        resolve({ code: null, signal: "spawn-error" });
      });
    });
    this.child.stdin.on("error", (error) => {
      this.failure = error;
      this.pending?.reject(error);
      this.child.kill("SIGKILL");
    });
    this.lines = readline.createInterface({ input: this.child.stdout });
    this.lines.on("line", (line) => {
      try {
        const envelope = JSON.parse(line);
        assert.ok(this.pending, "The private host sent an unsolicited envelope");
        this.lastReplyBytes = Buffer.byteLength(line);
        this.pending.resolve(envelope);
      } catch (error) {
        this.failure = error;
        this.pending?.reject(error);
        this.child.kill("SIGKILL");
      }
    });
  }
  async rpc(operation, args = {}) {
    assert.equal(this.pending, undefined, "Private host requests must be serialized");
    if (this.failure) throw this.failure;
    let timer;
    const reply = await new Promise((resolve, reject) => {
      const settle = (callback, value) => {
        clearTimeout(timer);
        this.pending = undefined;
        callback(value);
      };
      const pending = this.pending = {
        resolve: (value) => settle(resolve, value),
        reject: (error) => settle(reject, error),
      };
      timer = setTimeout(() => {
        this.failure = new Error(`Isolated Harness RPC timed out: ${operation}`);
        this.pending?.reject(this.failure);
        this.child.kill("SIGKILL");
      }, 10_000);
      this.child.stdin.write(JSON.stringify({ operation, args }) + "\n", (error) => {
        if (error && this.pending === pending) pending.reject(error);
      });
    });
    if (!reply.ok) throw new Error(reply.error);
    return reply.result;
  }
  async stop() {
    if (this.child.exitCode === null && this.child.signalCode === null) {
      if (this.pending || this.failure) this.child.kill("SIGKILL");
      else {
        try { await this.rpc("shutdown"); }
        catch { this.child.kill("SIGKILL"); }
      }
      this.child.stdin.end();
    }
    let outcome;
    try { outcome = await bounded(this.exited, 3000, "graceful host shutdown"); }
    catch {
      this.child.kill("SIGKILL");
      outcome = await bounded(this.exited, 2000, "forced host shutdown");
    }
    this.lines.close();
    return outcome;
  }
}

function sourceFixture(large) {
  const header = 'def greeting():\n    return "initial"\n\n# ';
  const implementation = header + "x".repeat(large ? 800_000 - header.length : 64) + "\n";
  const notes = large ? "Fixture documentation. " + "n".repeat(1_048_576 - 23) : "Fixture documentation.\n";
  const files = {
    "src/module.py": implementation,
    "docs/notes.txt": notes,
    "obsolete.txt": "Obsolete fixture note.\n",
    "settings/config.toml": "retain = true\n",
  };
  const replacement = implementation.replace('return "initial"', 'return "reviewed"');
  const proposal = {
    summary: "Update the fixture greeting and its notes.",
    changes: [
      { path: "src/module.py", expected_sha256: hash(implementation), content: replacement },
      { path: "docs/review.txt", expected_sha256: null, content: "Reviewed fixture note.\n" },
      { path: "obsolete.txt", expected_sha256: hash(files["obsolete.txt"]), content: null },
    ],
  };
  const script = [
    ["read_context", {}],
    ["search_history", { query: "previous decision" }],
    ["read_history", { offset: 0, limit: 2 }],
    ["list_files", {}],
    ["read_file", { path: "src/module.py", limit: 8000 }],
    ["search_files", { query: "initial", pathPrefix: "src/" }],
    ["report_plan", { summary: "Change the fixture greeting after inspection.", steps: ["Inspect the captured Python source", "Propose the greeting and note changes"] }],
    ["propose_changes", proposal],
  ];
  return { files, replacement, proposal, script };
}

async function fixture(t, { large = false } = {}) {
  await fs.mkdir(scratch, { recursive: true });
  const root = await fs.mkdtemp(path.join(scratch, "ipc-"));
  const source = sourceFixture(large), requests = [], providerErrors = [];
  let processHost, held;
  const server = http.createServer(async (request, response) => {
    try {
      let text = "";
      for await (const chunk of request) {
        text += chunk;
        assert.ok(Buffer.byteLength(text) <= 1_000_000, "The guarded SDK request exceeded its transport limit");
      }
      const body = JSON.parse(text);
      requests.push({ path: request.url, body });
      assert.equal(request.url, "/v1/chat/completions");
      assert.equal(body.model, "harness-fixture");
      assert.ok(body.max_tokens > 0);
      assert.deepEqual(body.tools.map((tool) => tool.function.name).sort(), exposedTools.map((name) => `harness__${name}`).sort());
      const turn = requests.length, call = source.script[turn - 1];
      assert.ok(turn <= source.script.length + 1, "The provider made an unexpected extra model turn");
      const base = { id: `loopback-${turn}`, object: "chat.completion.chunk", created: 0, model: "harness-fixture" };
      const emit = (delta, finish_reason = null) => response.write(`data: ${JSON.stringify({ ...base, choices: [{ index: 0, delta, finish_reason }] })}\n\n`);
      response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
      if (call) {
        emit({ role: "assistant", tool_calls: [{ index: 0, id: `loopback-tool-${turn}`, type: "function", function: { name: `harness__${call[0]}`, arguments: JSON.stringify(call[1]) } }] });
        emit({}, "tool_calls");
        response.end("data: [DONE]\n\n");
      } else {
        emit({ role: "assistant", content: finalText, reasoning_content: privateReasoning });
        // Keep the final turn incomplete after proposal publication. This makes
        // running/cancelled/failed export checks independent of polling speed.
        held = { finish(reason = "stop") { emit({}, reason); response.end("data: [DONE]\n\n"); } };
      }
    } catch (error) {
      providerErrors.push(error);
      if (!response.headersSent) response.writeHead(500, { "content-type": "application/json" });
      response.end(JSON.stringify({ error: { message: "Synthetic provider rejected the fixture request" } }));
    }
  });
  t.after(async () => {
    try { await processHost?.stop(); }
    finally {
      server.closeAllConnections();
      await bounded(new Promise((resolve) => server.close(resolve)), 2000, "loopback provider shutdown");
      await fs.rm(root, { recursive: true, force: true });
    }
  });
  await bounded(new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  }), 2000, "loopback provider startup");
  processHost = new PrivateHost(root);
  const rpc = (operation, args) => processHost.rpc(operation, args);
  await rpc("provider_preset_save", {
    name: "Isolated loopback fixture",
    config: { provider: "pcl-custom", model: "harness-fixture", api: "openai-completions", baseUrl: `http://127.0.0.1:${server.address().port}/v1`, key: "" },
    limits: { modelTurns: 0, toolCalls: 0, maximumOutputTokens: 0, wallTimeMs: 0, estimatedBudgetUsd: 0 },
  });
  const runArgs = {
    project: { id: projectId, name: "Generic Python fixture", instructions: "Use the existing Python conventions." },
    conversation: { id: conversationId, contextNote: "Retain the selected greeting behavior.", contextPaths: ["src/module.py"],
      messages: [{ role: "user", content: "A previous decision: preserve the public greeting API." }, { role: "assistant", content: "Preserve the captured source conventions." }] },
    conversationHistory: { schemaVersion: 1, startIndex: 0, totalMessages: 2, truncated: false, messages: [
      { id: "history-user", role: "user", content: "A previous decision: preserve the public greeting API.", jobId: null },
      { id: "history-assistant", role: "assistant", content: "Preserve the captured source conventions.", jobId: null },
    ] },
    mode: "execute", request: "Inspect the generic project and propose the reviewed changes.",
    sourceFiles: Object.entries(source.files).map(([path, content]) => ({ path, size: Buffer.byteLength(content), sha256: hash(content), data: Buffer.from(content).toString("base64") })),
    protectedPaths: ["settings/config.toml"],
  };
  return {
    root, rpc, source, runArgs, requests, providerErrors,
    get host() { return processHost; },
    async waitForProposal(jobId) {
      return until(async () => {
        assert.deepEqual(providerErrors, [], "The synthetic provider rejected an SDK request");
        const view = await rpc("job_read", { jobId });
        if (["failed", "cancelled"].includes(view.summary.status))
          assert.fail(`Harness stopped before its final stream: ${JSON.stringify(view.error)}`);
        return held && view.output === finalText && view.activity.length === source.script.length && view.activity.every((operation) => operation.status === "completed") && view;
      }, "proposal publication and visible live output");
    },
    finish(reason) { assert.ok(held, "The final SDK stream was not reached"); held.finish(reason); },
    async restart() {
      assert.deepEqual(await processHost.stop(), { code: 0, signal: null });
      processHost = new PrivateHost(root);
      await rpc("status");
    },
  };
}

const ownership = (jobId) => ({ jobId, projectId, conversationId });
const approval = (jobId, review) => ({ ...ownership(jobId), proposalId: review.id, expectedDigest: review.digest });

test("private host admits a large generic snapshot, runs the real SDK and persists a digest-bound native handoff", { timeout: 45_000 }, async (t) => {
  const f = await fixture(t, { large: true });
  const requestBytes = Buffer.byteLength(JSON.stringify({ operation: "rh_code_run", args: f.runArgs }));
  assert.ok(requestBytes > 1_000_000 && requestBytes < 24 * 1024 * 1024);
  const { jobId, contextProjection } = await f.rpc("rh_code_run", f.runArgs);
  assert.deepEqual(contextProjection, { retained: 2, omitted: 0, method: "recent-message projection" });
  const queued = await f.rpc("job_read", { jobId });
  assert.equal(queued.summary.status, "queued");
  assert.equal(queued.summary.workflow, "harness");
  assert.equal(queued.output, "");
  assert.deepEqual(queued.activity, []);
  assert.deepEqual(await f.rpc("idle_prepare"), { busy: true });
  await new Promise((resolve) => setTimeout(resolve, 75));
  assert.equal(f.requests.length, 0, "Preparation and native admission must not contact the provider");
  for (const badOwner of [{ projectId: "another-project" }, { conversationId: "another-conversation" }])
    await assert.rejects(f.rpc("rh_code_start", { ...ownership(jobId), ...badOwner }), /belongs to another project or conversation/);
  assert.equal(f.requests.length, 0);
  await assert.rejects(f.rpc("rh_code_harness_export", { ...ownership(jobId), proposalId: "a".repeat(32), expectedDigest: "b".repeat(64) }), /Only a successfully completed job/);
  const jobDirectory = path.join(f.root, "jobs", jobId);
  const before = JSON.parse(await fs.readFile(path.join(jobDirectory, "job.json"), "utf8"));
  assert.equal(before.input.harness.pluginId, "rh.project");
  assert.equal(before.input.harness.source.fileCount, 4);
  assert.ok(before.input.harness.source.totalBytes > 1_000_000);
  assert.equal(before.input.harness.source.directory, "source");
  const snapshot = JSON.parse(await fs.readFile(path.join(jobDirectory, "control/harness-snapshot.json"), "utf8"));
  assert.deepEqual(snapshot.protectedPaths, ["settings/config.toml"]);
  assert.equal(snapshot.manifest.find((file) => file.path === "docs/notes.txt").size, 1_048_576);
  await assert.rejects(fs.access(path.join(jobDirectory, "workspace/control/harness-snapshot.json")), { code: "ENOENT" });

  await f.rpc("rh_code_start", ownership(jobId));
  const running = await f.waitForProposal(jobId);
  assert.equal(running.summary.status, "running");
  assert.equal(running.summary.artifactState, "draft_ready");
  assert.equal(running.result, null);
  assert.deepEqual(running.activity.map((operation) => operation.name), f.source.script.map(([name]) => `harness.${name}`));
  assert.equal(running.activity.find((operation) => operation.name === "harness.read_file").path, "src/module.py");
  assert.deepEqual(running.activity.find((operation) => operation.name === "harness.report_plan").steps, f.source.script.find(([name]) => name === "report_plan")[1].steps);
  assert.equal(JSON.stringify(running).includes(privateReasoning), false);
  assert.equal(JSON.stringify(running).includes("pcl-local-no-auth"), false);
  assert.ok(f.requests.some(request => request.body.messages.some((message) => message.role === "tool" && message.content.includes(hash(f.source.files["src/module.py"])) && message.content.includes('return \\"initial\\"'))));
  assert.ok(f.requests.some(request => request.body.messages.some(message => message.role === "tool" && message.content.includes("history-user") && message.content.includes("previous decision"))));
  assert.ok(f.requests.some(request => request.body.messages.some(message => message.role === "tool" && message.content.includes("Use the existing Python conventions."))));
  assert.match(JSON.stringify(f.requests[0].body.messages), /Generic Python fixture/);
  assert.match(JSON.stringify(f.requests[0].body.messages), /Preserve the captured source conventions/);
  const reviews = await f.rpc("rh_code_harness_reviews", ownership(jobId));
  assert.equal(reviews.total, 1);
  assert.equal(reviews.proposals.length, 1);
  const review = reviews.proposals[0], grant = approval(jobId, review);
  assert.equal(review.status, "pending");
  assert.equal(review.changeCount, 3);
  assert.match(review.digest, /^[a-f0-9]{64}$/);
  assert.equal(review.changes.find((change) => change.path === "src/module.py").beforeTruncated, true);
  await assert.rejects(f.rpc("rh_code_harness_export", grant), /Only a successfully completed job/);
  await assert.rejects(f.rpc("rh_code_harness_applied", grant), /Only a completed job/);
  for (const operation of ["rh_code_harness_reviews", "rh_code_harness_export", "rh_code_harness_applied"])
    for (const badOwner of [{ projectId: "another-project" }, { conversationId: "another-conversation" }])
      await assert.rejects(f.rpc(operation, { ...grant, ...badOwner }), /belongs to another project or conversation/);

  f.finish();
  const completed = await until(async () => {
    const view = await f.rpc("job_read", { jobId });
    return ["completed", "failed", "cancelled"].includes(view.summary.status) && view;
  }, "successful SDK completion");
  assert.equal(completed.summary.status, "completed", JSON.stringify(completed.error));
  assert.equal(completed.output, finalText);
  assert.equal(completed.result.text, finalText);
  assert.equal(completed.result.toolCalls, f.source.script.length);
  assert.equal(completed.result.modelTurns, f.source.script.length + 1);
  assert.equal(completed.result.approvalGranted, false);
  assert.equal(completed.result.buildValidated, false);
  assert.equal(completed.result.runtimeValidated, false);
  assert.equal(completed.summary.verificationState, "not_run");
  assert.equal(f.requests.length, f.source.script.length + 1);
  assert.deepEqual(f.providerErrors, []);
  for (const operation of ["rh_code_harness_export", "rh_code_harness_applied"])
    await assert.rejects(f.rpc(operation, { ...grant, expectedDigest: "0".repeat(64) }), /exact.*digest/);
  const exported = await f.rpc("rh_code_harness_export", grant);
  assert.ok(f.host.lastReplyBytes > 1_000_000 && f.host.lastReplyBytes < 24 * 1024 * 1024, "Private export must exceed the old reply budget successfully");
  assert.equal(exported.projectId, projectId);
  assert.equal(exported.conversationId, conversationId);
  assert.equal(exported.sourceFingerprint, before.input.harness.source.fingerprint);
  assert.equal(exported.digest, review.digest);
  assert.equal(exported.files.length, 3);
  const changed = exported.files.find((file) => file.path === "src/module.py");
  assert.equal(changed.expectedSha256, hash(f.source.files[changed.path]));
  assert.equal(Buffer.from(changed.data, "base64").toString(), f.source.replacement);
  assert.deepEqual(exported.files.find((file) => file.path === "obsolete.txt"), { path: "obsolete.txt", expectedSha256: hash(f.source.files["obsolete.txt"]), data: null, delete: true });
  assert.equal(exported.files.find((file) => file.path === "docs/review.txt").expectedSha256, null);
  for (const [name, original] of Object.entries(f.source.files))
    assert.equal(await fs.readFile(path.join(jobDirectory, "workspace/source", name), "utf8"), original, "Harness proposals must preserve every frozen input byte");
  const jobText = await fs.readFile(path.join(jobDirectory, "job.json"), "utf8");
  const persisted = JSON.parse(jobText);
  assert.ok(Buffer.byteLength(jobText) < 40_000, "Inspection and proposal receipts must remain compact");
  assert.equal(persisted.providerState.livePi.activity.phase, "idle");
  const readReceipt = persisted.operations.find((operation) => operation.name === "harness.read_file").result;
  assert.equal(readReceipt.contentStored, false);
  assert.equal(Object.hasOwn(readReceipt, "content"), false);
  assert.equal(readReceipt.sha256, changed.expectedSha256);
  const eventPath = path.join(jobDirectory, "events.jsonl");
  const events = await fs.readFile(eventPath, "utf8");
  assert.equal(events.match(/pi\.tool\.started/g)?.length, f.source.script.length);
  assert.equal(events.match(/pi\.tool\.completed/g)?.length, f.source.script.length);
  assert.equal(events.includes('return "initial"'), false);
  assert.equal(events.includes(privateReasoning), false);

  const acknowledged = await f.rpc("rh_code_harness_applied", grant);
  assert.equal(acknowledged.status, "applied");
  assert.deepEqual(await f.rpc("rh_code_harness_applied", grant), acknowledged);
  assert.equal((await f.rpc("rh_code_harness_reviews", ownership(jobId))).proposals[0].status, "applied");
  assert.equal((await fs.readFile(eventPath, "utf8")).match(/harness\.proposal\.applied/g)?.length, 1);
  await f.restart();
  const restored = await f.rpc("rh_code_harness_reviews", ownership(jobId));
  assert.equal(restored.proposals[0].status, "applied");
  assert.equal(restored.proposals[0].appliedAt, acknowledged.appliedAt);
  assert.deepEqual(await f.rpc("rh_code_harness_applied", grant), acknowledged);
  assert.deepEqual(await f.rpc("rh_code_harness_export", grant), exported);
  assert.deepEqual(await f.rpc("rh_code_harness_reviews", { jobId, projectId }), restored, "Project-level historical review lookup may omit the conversation");
  assert.equal((await f.rpc("job_read", { jobId })).summary.status, "completed");
  assert.equal((await fs.readFile(eventPath, "utf8")).match(/harness\.proposal\.applied/g)?.length, 1);
  assert.equal(f.requests.length, f.source.script.length + 1, "Review recovery must not restart the model");
});

for (const status of ["cancelled", "failed"])
  test(`private host withholds an existing proposal after the SDK job is ${status}`, { timeout: 30_000 }, async (t) => {
    const f = await fixture(t);
    const { jobId } = await f.rpc("rh_code_run", f.runArgs);
    await f.rpc("rh_code_start", ownership(jobId));
    await f.waitForProposal(jobId);
    const review = (await f.rpc("rh_code_harness_reviews", ownership(jobId))).proposals[0];
    if (status === "cancelled") assert.equal(await f.rpc("job_cancel", { jobId }), true);
    else f.finish("length");
    const stopped = await until(async () => {
      const view = await f.rpc("job_read", { jobId });
      return ["completed", "failed", "cancelled"].includes(view.summary.status) && view;
    }, `${status} SDK completion`);
    assert.equal(stopped.summary.status, status, JSON.stringify(stopped.error));
    if (status === "failed") assert.equal(stopped.error.code, "MODEL_OUTPUT_LIMIT");
    const grant = approval(jobId, review);
    await assert.rejects(f.rpc("rh_code_harness_export", grant), /Only a successfully completed job/);
    await assert.rejects(f.rpc("rh_code_harness_applied", grant), /Only a completed job/);
    assert.equal((await f.rpc("rh_code_harness_reviews", ownership(jobId))).proposals[0].status, "pending");
    assert.equal(f.requests.length, f.source.script.length + 1);
    assert.deepEqual(f.providerErrors, []);
  });
