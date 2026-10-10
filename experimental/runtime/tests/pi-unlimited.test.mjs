/** Real provider serialization with an offline session driver. The driver can
 * cross the old count boundaries deterministically without any paid service,
 * domain mutation, credential discovery or user workspace. */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import * as sdk from "@earendil-works/pi-coding-agent";
import { LivePiProvider } from "../src/providers/pi-live.mjs";
import { normalizePiSelection } from "../src/providers/pi-selection.mjs";
import { AgentRuntime } from "../src/runtime.mjs";

const unlimited = {
  modelTurns: 0,
  toolCalls: 0,
  maximumOutputTokens: 0,
  wallTimeMs: 0,
  estimatedBudgetUsd: 0,
};
const scratch = fileURLToPath(
  new URL("../../../work/ai-unlimited-2026-10-10/fixtures/", import.meta.url),
);
const tools = {
  describe: () => [
    {
      name: "workspace.read",
      description: "Offline receipt fixture",
      inputSchema: {
        type: "object",
        properties: { path: { type: "string" } },
        required: ["path"],
      },
    },
  ],
  get: () => ({
    validate() {},
    replaySafety: "read-only",
    execute: async () => ({ fixture: true }),
  }),
};

function response() {
  const base = {
    id: "offline",
    object: "chat.completion.chunk",
    created: 0,
    model: "offline",
  };
  const chunks = [
    {
      ...base,
      choices: [
        {
          index: 0,
          delta: { role: "assistant", content: "Offline response" },
          finish_reason: null,
        },
      ],
    },
    { ...base, choices: [{ index: 0, delta: {}, finish_reason: "stop" }] },
  ];
  return new Response(
    chunks.map((v) => "data: " + JSON.stringify(v) + "\n\n").join("") +
      "data: [DONE]\n\n",
    { headers: { "content-type": "text/event-stream" } },
  );
}

async function fixture(
  t,
  { limits = unlimited, turns = 1, calls = 0, blocked = false } = {},
) {
  await fs.mkdir(scratch, { recursive: true });
  const root = await fs.mkdtemp(path.join(scratch, "run-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const selection = await normalizePiSelection({
    provider: "pcl-custom",
    model: "offline",
    api: "openai-completions",
    baseUrl: "http://127.0.0.1:1234/v1",
    cost: { input: 1, output: 1 },
  });
  const requests = [];
  let entered;
  const started = new Promise((resolve) => {
    entered = resolve;
  });
  const provider = new LivePiProvider(
    { enabled: true, getSelection: () => selection, getLimits: () => limits },
    {
      loadSdk: async () => ({
        ...sdk,
        // Keep the pinned ModelRuntime/serializer and host tool wrapper. Replace
        // only the nondeterministic model-driven loop with this explicit driver.
        createAgentSession: async (options) => {
          let notify = () => {};
          const session = {
            agent: {},
            messages: [],
            subscribe(callback) {
              notify = callback;
              return () => {
                notify = () => {};
              };
            },
            async prompt() {
              for (let i = 0; i < calls; i++)
                await options.customTools[0].execute(
                  "offline-tool-" + i,
                  { path: "fixture.txt" },
                  new AbortController().signal,
                );
              for (let i = 0; i < turns; i++) {
                const stream = await session.agent.streamFunction(
                  options.model,
                  {
                    systemPrompt: "Offline contract",
                    messages: [
                      { role: "user", content: "One word", timestamp: 0 },
                    ],
                  },
                );
                const message = await stream.result();
                session.messages.push(message);
                notify({ type: "message_end", message });
                if (message.stopReason !== "stop") break;
              }
            },
            abort: async () => {},
            dispose() {},
            getLastAssistantText: () => "Offline response",
          };
          return { session };
        },
      }),
      fetch: async (_url, options) => {
        requests.push(JSON.parse(options.body));
        entered();
        if (blocked)
          return new Promise((_resolve, reject) => {
            const abort = () =>
              reject(new DOMException("Offline cancellation", "AbortError"));
            if (options.signal.aborted) abort();
            else
              options.signal.addEventListener("abort", abort, { once: true });
          });
        // A zero wall-time must not schedule an immediate abort while SDK work is pending.
        await new Promise((resolve) => setTimeout(resolve, 5));
        return response();
      },
    },
  );
  const runtime = await new AgentRuntime({
    root,
    tools,
    providers: [provider],
  }).init();
  t.after(() => runtime.shutdown());
  const job = await runtime.createJob({
    profile: "maker",
    provider: provider.id,
  });
  return { runtime, job, requests, started };
}

test(
  "all-zero budgets exceed default turns and the legacy 200-tool cap with durable receipts",
  { timeout: 15000 },
  async (t) => {
    const { runtime, job, requests } = await fixture(t, {
      turns: 34,
      calls: 201,
    });
    const result = await runtime.start(job.id);
    assert.equal(result.status, "completed", JSON.stringify(result.error));
    assert.equal(requests.length, 34);
    assert.ok(
      requests.every(
        (request) => request.max_tokens > 8192 && request.max_tokens <= 32768,
      ),
    );
    assert.equal(result.result.toolCalls, 201);
    assert.equal(result.result.modelTurns, 34);
    assert.ok(result.result.reservedEstimatedUsd > 0);
    const stored = await runtime.getJob(job.id);
    assert.equal(stored.operations.length, 201);
    assert.ok(
      stored.operations.every((operation) => operation.status === "completed"),
    );
  },
);

for (const scenario of [
  {
    name: "model turns",
    limits: { modelTurns: 1 },
    turns: 2,
    code: "MODEL_TURN_LIMIT",
  },
  {
    name: "tool calls",
    limits: { toolCalls: 1 },
    calls: 2,
    code: "MODEL_TOOL_LIMIT",
  },
  {
    name: "estimated spend",
    limits: { estimatedBudgetUsd: 0.01 },
    code: "MODEL_SPEND_LIMIT",
  },
  {
    name: "task duration",
    limits: { wallTimeMs: 25 },
    blocked: true,
    code: "MODEL_TIMEOUT",
  },
])
  test(`positive ${scenario.name} budgets still stop work`, async (t) => {
    const { runtime, job } = await fixture(t, {
      ...scenario,
      limits: { ...unlimited, ...scenario.limits },
    });
    const result = await runtime.start(job.id);
    assert.equal(result.status, "failed");
    assert.equal(result.error.code, scenario.code);
  });

test("unlimited duration remains manually cancellable", async (t) => {
  const { runtime, job, started, requests } = await fixture(t, {
    blocked: true,
  });
  const run = runtime.start(job.id);
  await started;
  await new Promise((resolve) => setTimeout(resolve, 25));
  assert.equal((await runtime.getJob(job.id)).status, "running");
  assert.equal(await runtime.cancel(job.id), true);
  assert.equal((await run).status, "cancelled");
  assert.equal(requests.length, 1);
});
