/** Real Pi SDK session + serializer + tool loop, with a scripted in-process SSE
 * transport. No network, account, user source, shell or generated code is used.
 */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { AgentRuntime } from "../src/runtime.mjs";
import { ToolRegistry } from "../src/tools.mjs";
import { registerHarnessTools } from "../src/harness-tools.mjs";
import { LivePiProvider } from "../src/providers/pi-live.mjs";
import { normalizePiSelection } from "../src/providers/pi-selection.mjs";
import { RuntimeError } from "../src/errors.mjs";
import { HarnessService } from "../../../code/harness-service.mjs";
import { captureNativeSnapshot, stageNativeSnapshot } from "../../../code/harness-snapshot.mjs";

const scratch = fileURLToPath(new URL("../../../work/harness-2026-10-12/provider-tests/", import.meta.url));
const source = "class Example { String message = \"initial\"; }\n";
const hash = createHash("sha256").update(source).digest("hex");
const unlimited = { modelTurns: 0, toolCalls: 0, maximumOutputTokens: 0, wallTimeMs: 0, estimatedBudgetUsd: 0 };
const calls = [
  ["harness.list_files", {}],
  ["harness.read_file", { path: "src/Example.java" }],
  ["harness.search_files", { query: "initial", pathPrefix: "src/" }],
  ["harness.report_plan", { summary: "Change the fixture greeting.", steps: ["Inspect the current source", "Propose a reviewed greeting change"] }],
  ["harness.propose_changes", { summary: "Update the greeting.", changes: [{ path: "src/Example.java", expected_sha256: hash, content: source.replace("initial", "reviewed") }] }],
];
const toolName = (name) => name.replace(".", "__");
function sse(turn, call, id = `offline-tool-${turn}`) {
  const base = { id: `offline-${turn}`, object: "chat.completion.chunk", created: 0, model: "harness-fixture" };
  const delta = call
    ? { role: "assistant", tool_calls: [{ index: 0, id, type: "function", function: { name: toolName(call[0]), arguments: JSON.stringify(call[1]) } }] }
    : { role: "assistant", content: "The proposal is ready for review." };
  const chunks = [
    { ...base, choices: [{ index: 0, delta, finish_reason: null }] },
    { ...base, choices: [{ index: 0, delta: {}, finish_reason: call ? "tool_calls" : "stop" }] },
  ];
  return new Response(chunks.map((value) => `data: ${JSON.stringify(value)}\n\n`).join("") + "data: [DONE]\n\n",
    { headers: { "content-type": "text/event-stream" } });
}
async function fixture(t, { mode = "execute", script = calls, limits = unlimited, overrides = {}, transport, key = "", realService = false, sourceText = source, toolId } = {}) {
  await fs.mkdir(scratch, { recursive: true });
  const root = await fs.mkdtemp(path.join(scratch, "run-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const proposals = [], invoked = [];
  const service = {
    async listFiles() { invoked.push("list"); return { files: [{ path: "src/Example.java", sha256: hash, size: Buffer.byteLength(source) }] }; },
    async readFile(ctx, args) { invoked.push("read"); return { path: args.path, sha256: hash, content: source }; },
    async searchFiles() { invoked.push("search"); return { matches: [{ path: "src/Example.java", line: 1, text: source.trim() }] }; },
    async reportPlan(_ctx, args) { invoked.push("plan"); return { ...args, unverified: true }; },
    async proposeChanges(ctx, args) { invoked.push("propose"); proposals.push(args); return { proposalId: ctx.operationId, summary: args.summary, status: "pending", changeCount: args.changes.length, originalSourceModified: false }; },
    ...overrides,
  };
  const selection = await normalizePiSelection({ provider: "pcl-custom", model: "harness-fixture", api: "openai-completions", baseUrl: "http://127.0.0.1:18080/v1", key });
  const requests = [];
  const provider = new LivePiProvider({ enabled: true, getSelection: () => selection, limits }, {
    // The real SDK is loaded by LivePiProvider. Only its guarded transport is replaced.
    fetch: async (_url, options) => {
      const request = JSON.parse(options.body);
      requests.push(request);
      assert.equal(request.model, "harness-fixture");
      assert.ok(request.max_tokens > 0);
      const exposed = request.tools.map((tool) => tool.function.name);
      const expected = calls.map(([name]) => toolName(name)).filter((name) =>
        mode === "execute" || ["harness__list_files", "harness__read_file", "harness__search_files"].includes(name) ||
        (mode === "plan" && name === "harness__report_plan"));
      assert.deepEqual(exposed.sort(), expected.sort());
      return transport ? transport(request, options, requests.length) : sse(requests.length, script[requests.length - 1], toolId?.(requests.length));
    },
  });
  const tools = new ToolRegistry();
  const runtime = await new AgentRuntime({ root, tools, providers: [provider] }).init();
  t.after(() => runtime.shutdown());
  let harnessService;
  if (realService) {
    harnessService = new HarnessService({ store: runtime.store });
    const delegated = {};
    for (const [method, label] of [["listFiles", "list"], ["readFile", "read"], ["searchFiles", "search"], ["reportPlan", "plan"], ["proposeChanges", "propose"]])
      delegated[method] = (ctx, args) => {
        invoked.push(label);
        if (method === "proposeChanges") proposals.push(args);
        return harnessService[method](ctx, args);
      };
    registerHarnessTools(tools, delegated);
  } else registerHarnessTools(tools, service);
  const snapshot = captureNativeSnapshot([{ path: "src/Example.java", sha256: createHash("sha256").update(sourceText).digest("hex"), data: Buffer.from(sourceText).toString("base64") }]);
  const job = await runtime.createJob({ profile: "harness", provider: provider.id, input: {
    prompt: "Change the greeting after inspection.",
    harness: { schemaVersion: 1, mode, pluginId: "rh.project", projectId: "fixture", conversationId: "conversation-fixture", source: snapshot.descriptor },
  } });
  const workspace = await runtime.store.workspace(job.id);
  await stageNativeSnapshot(runtime.store, job, snapshot);
  return { root, runtime, job, requests, invoked, proposals, workspace, tools, harnessService };
}

test("real Pi session inspects, plans and proposes through the generic harness tools", async (t) => {
  const { runtime, job, requests, invoked, proposals, workspace, harnessService } = await fixture(t, { realService: true });
  const result = await runtime.start(job.id);
  assert.equal(result.status, "completed", JSON.stringify(result.error));
  assert.equal(requests.length, 6);
  assert.deepEqual(invoked, ["list", "read", "search", "plan", "propose"]);
  assert.equal(result.result.toolCalls, 5);
  assert.equal(result.result.modelTurns, 6);
  assert.equal(result.result.text, "The proposal is ready for review.");
  assert.equal(result.result.approvalGranted, false);
  assert.equal(result.result.buildValidated, false);
  assert.equal(proposals[0].changes[0].expected_sha256, hash);
  const review = await harnessService.getReview(job.id, result.operations.at(-1).result.proposalId);
  assert.equal(review.status, "pending");
  assert.equal(review.changes[0].expectedSha256, hash);
  assert.equal((await workspace.read("source/src/Example.java")).content, source);
  assert.equal(result.operations.length, 5);
  assert.ok(result.operations.every((operation) => operation.status === "completed"));
  assert.equal((await runtime.getPublicJobSummary(job.id)).artifactState, "draft_ready");
  assert.equal(result.providerState.livePi.activity.phase, "idle");
  assert.equal(result.providerState.livePi.activity.textChars, result.result.text.length);
  assert.equal(result.providerState.livePi.output, result.result.text);
  const events = (await (await runtime.store.files(job.id)).read("events.jsonl")).content;
  assert.equal(events.match(/pi\.tool\.started/g)?.length, 5);
  assert.equal(events.match(/pi\.tool\.completed/g)?.length, 5);
  assert.equal(events.includes(source.trim()), false);
  assert.equal(events.includes("pcl-local-no-auth"), false);
  // Serializer includes the actual tool results in subsequent model requests.
  assert.ok(requests[2].messages.some((message) => message.role === "tool" && message.content.includes(hash)));
});

for (const mode of ["ask", "plan"])
  test(`real Pi ${mode} session receives only its captured tool permissions`, async (t) => {
    const script = mode === "ask" ? calls.slice(0, 3) : calls.slice(0, 4);
    const { runtime, job, proposals } = await fixture(t, { mode, script });
    const result = await runtime.start(job.id);
    assert.equal(result.status, "completed", JSON.stringify(result.error));
    assert.equal(proposals.length, 0);
    assert.equal(result.operations.length, script.length);
  });

test("real Pi execute loop cannot propose before recording a plan", async (t) => {
  const { runtime, job, requests, proposals } = await fixture(t, { script: [calls[4]] });
  const result = await runtime.start(job.id);
  // Pi reports the failed tool call to the model, which can finish without a proposal.
  assert.equal(result.status, "completed", JSON.stringify(result.error));
  assert.equal(proposals.length, 0);
  assert.equal(result.operations.length, 0);
  assert.equal((await runtime.getPublicJobSummary(job.id)).artifactState, "none");
  assert.ok(requests[1].messages.some((message) => message.role === "tool" && message.content.includes("Record a plan")));
});

test("positive tool budgets still interrupt a real harness loop", async (t) => {
  const { runtime, job, invoked } = await fixture(t, { limits: { ...unlimited, toolCalls: 1 } });
  const result = await runtime.start(job.id);
  assert.equal(result.status, "failed");
  assert.equal(result.error.code, "MODEL_TOOL_LIMIT");
  assert.deepEqual(invoked, ["list"]);
});

test("zero duration remains cancellable during a real registered tool", async (t) => {
  let enter;
  const entered = new Promise((resolve) => { enter = resolve; });
  const { runtime, job } = await fixture(t, { script: [calls[1]], overrides: {
    async readFile(ctx) {
      enter();
      await new Promise((_resolve, reject) => {
        const abort = () => reject(new RuntimeError("CANCELLED", "Fixture cancelled"));
        if (ctx.signal.aborted) abort();
        else ctx.signal.addEventListener("abort", abort, { once: true });
      });
    },
  } });
  const run = runtime.start(job.id);
  await entered;
  const active = await runtime.getJob(job.id);
  assert.equal(active.providerState.livePi.activity.phase, "tool");
  assert.equal(active.providerState.livePi.activity.toolName, "harness.read_file");
  assert.equal(await runtime.cancel(job.id), true);
  assert.equal((await run).status, "cancelled");
  assert.equal((await runtime.getJob(job.id)).operations[0].status, "failed");
});

test("generic tool validation refuses path escapes and malformed change authority", async (t) => {
  const { tools } = await fixture(t);
  for (const path of ["/etc/passwd", "../source", "src\\Example.java", "C:/source", "src//Example.java"])
    assert.throws(() => tools.get("harness.read_file", ["harness.read_file"]).validate({ path }), { code: "UNSAFE_PATH" });
  const propose = tools.get("harness.propose_changes", ["harness.propose_changes"]);
  for (const change of [
    { path: "src/Example.java", expected_sha256: null, content: null },
    { path: "src/Example.java", expected_sha256: "bad", content: "text" },
    { path: "src/Example.java", content: "text" },
    { path: "src/Example.java", expected_sha256: null, content: "text", approve: true },
  ]) assert.throws(() => propose.validate({ summary: "Invalid fixture", changes: [change] }), { code: "INVALID_ARGUMENTS" });
});

test("runtime rejects unavailable harness capabilities even when a provider bypasses tool discovery", async (t) => {
  const { runtime, tools } = await fixture(t);
  const provider = {
    id: "offline-permission-check",
    async run(ctx) { return ctx.invoke("attempt", ctx.job.input.tool, ctx.job.input.args); },
  };
  runtime.providers.set(provider.id, provider);
  for (const [mode, tool, args] of [
    ["ask", "harness.report_plan", calls[3][1]],
    ["ask", "harness.propose_changes", calls[4][1]],
    ["plan", "harness.propose_changes", calls[4][1]],
    ["execute", "workspace.write", { path: "scratch/file", content: "denied" }],
  ]) {
    const job = await runtime.createJob({ profile: "harness", provider: provider.id, input: { harness: { mode }, tool, args } });
    const result = await runtime.start(job.id);
    assert.equal(result.status, "failed");
    assert.equal(result.error.code, "TOOL_DENIED");
    assert.equal(result.operations.length, 0);
  }
  assert.equal(tools.describe(["workspace.write"]).length, 0);
  for (const mode of [undefined, "unrestricted"]) {
    const job = await runtime.createJob({ profile: "harness", provider: provider.id, input: { harness: { mode } } });
    const result = await runtime.start(job.id);
    assert.equal(result.error.code, "INVALID_MODE");
  }
});

test("real incomplete Pi stream exposes bounded visible text and withholds split keys and reasoning", async (t) => {
  const key = "offline-secret-stream-token";
  const reasoning = "PRIVATE_REASONING_FIXTURE";
  let controller;
  const encode = (delta) => new TextEncoder().encode(`data: ${JSON.stringify({
    id: "partial", object: "chat.completion.chunk", created: 0, model: "harness-fixture",
    choices: [{ index: 0, delta, finish_reason: null }],
  })}\n\n`);
  const { runtime, job } = await fixture(t, { mode: "ask", key, transport: async (_request, options) => {
    const body = new ReadableStream({
      start(target) {
        controller = target;
        const abort = () => target.error(new DOMException("Offline stream cancelled", "AbortError"));
        options.signal.addEventListener("abort", abort, { once: true });
        target.enqueue(encode({ role: "assistant", reasoning_content: reasoning, content: "Visible response " + key.slice(0, 8) }));
      },
    });
    return new Response(body, { headers: { "content-type": "text/event-stream" } });
  } });
  const run = runtime.start(job.id);
  async function waitOutput(expected) {
    for (let attempt = 0; attempt < 200; attempt++) {
      const current = await runtime.getJob(job.id);
      if (current.providerState.livePi?.output === expected) return current;
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    assert.fail("Live output was not projected while the stream was incomplete");
  }
  const first = await waitOutput("Visible response ");
  assert.equal(first.status, "running");
  assert.equal(first.providerState.livePi.activity.phase, "responding");
  assert.equal(JSON.stringify(first).includes(reasoning), false);
  assert.equal(JSON.stringify(first).includes(key.slice(0, 8)), false);
  controller.enqueue(encode({ content: key.slice(8) + " continues.", reasoning_content: reasoning }));
  const second = await waitOutput("Visible response [redacted] continues.");
  assert.equal(JSON.stringify(second).includes(key), false);
  assert.equal(JSON.stringify(second).includes(reasoning), false);
  controller.enqueue(encode({ content: "x".repeat(20_000) }));
  const bounded = await waitOutput(("Visible response [redacted] continues." + "x".repeat(20_000)).slice(0, 16_000));
  assert.equal(bounded.providerState.livePi.output.length, 16_000);
  assert.equal(JSON.stringify(bounded).includes(key), false);
  assert.equal(await runtime.cancel(job.id), true);
  assert.equal((await run).status, "cancelled");
});

test("real Pi substantial reads keep compact durable receipts and reconstruct same-ID source results", async (t) => {
  const read = ["harness.read_file", { path: "src/Example.java", limit: 16000 }];
  const f = await fixture(t, { mode: "ask", realService: true, sourceText: "substantial source ".repeat(1500),
    script: Array.from({ length: 12 }, () => read), toolId: (turn) => turn <= 2 ? "replayed-large-read" : `large-read-${turn}` });
  const result = await f.runtime.start(f.job.id);
  assert.equal(result.status, "completed", JSON.stringify(result.error));
  assert.equal(result.result.toolCalls, 12);
  assert.equal(result.operations.length, 11);
  assert.ok(result.operations.every((operation) => operation.result.contentStored === false && operation.result.charactersRead === 16000 && !Object.hasOwn(operation.result, "content")));
  assert.ok(Buffer.byteLength(JSON.stringify(result.operations)) < 10_000);
  const toolMessages = f.requests.at(-1).messages.filter((message) => message.role === "tool");
  assert.equal(toolMessages.length, 12);
  assert.ok(toolMessages.every((message) => JSON.parse(message.content).content.length === 16000));
  const events = (await (await f.runtime.store.files(f.job.id)).read("events.jsonl")).content;
  assert.equal(events.match(/"tool.replayed"/g)?.length, 1);
});

test("real Pi same-ID inspection replay refuses changed frozen bytes instead of serving a cached excerpt", async (t) => {
  let workspace;
  const read = ["harness.read_file", { path: "src/Example.java" }];
  const f = await fixture(t, { mode: "ask", realService: true, script: [read, read], transport: async (_request, _options, turn) => {
    if (turn === 2) await fs.writeFile(await workspace.resolve("source/src/Example.java"), "changed private snapshot");
    return sse(turn, turn <= 2 ? read : undefined, "same-inspection");
  } });
  workspace = f.workspace;
  const result = await f.runtime.start(f.job.id);
  assert.equal(result.status, "completed", JSON.stringify(result.error));
  assert.equal(result.operations.length, 1);
  assert.equal(result.operations[0].result.sha256, hash);
  assert.ok(f.requests[2].messages.filter((message) => message.role === "tool").at(-1).content.includes("immutable private snapshot changed"));
  const events = (await (await f.runtime.store.files(f.job.id)).read("events.jsonl")).content;
  assert.ok(events.includes("tool.replay_failed"));
});
