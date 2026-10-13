/** Pinned real SDK and real provider serializer with an in-process SSE transport.
 * No user credentials, paid endpoints, native apps or project folders are used. */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { AgentRuntime } from "../src/runtime.mjs";
import { ToolRegistry } from "../src/tools.mjs";
import { registerHarnessTools } from "../src/harness-tools.mjs";
import { registerVisualTools } from "../src/visual-tools.mjs";
import { LivePiProvider } from "../src/providers/pi-live.mjs";
import { normalizePiSelection } from "../src/providers/pi-selection.mjs";
import { HarnessService } from "../../../code/harness-service.mjs";
import { VisualService } from "../../../code/visual-service.mjs";
import { createPixel } from "../../../code/visual-assets.mjs";
import {
  captureNativeSnapshot,
  stageNativeSnapshot,
  sha256,
} from "../../../code/harness-snapshot.mjs";
import {
  HarnessToolCatalog,
  captureHarnessToolScope,
} from "../../../code/harness-tool-catalog.mjs";

const scratch = fileURLToPath(
  new URL(
    "../../../work/visual-assets-2026-10-13/provider-tests/",
    import.meta.url,
  ),
);
const pixelPath = "textures/example.rhtexture.json",
  modelPath = "models/example.rhmodel.json";
const unlimited = {
  modelTurns: 0,
  toolCalls: 0,
  maximumOutputTokens: 0,
  wallTimeMs: 0,
  estimatedBudgetUsd: 0,
};
const plan = [
  "harness.report_plan",
  {
    summary: "Create and inspect visual drafts",
    steps: [
      "Create texture",
      "Inspect rendered pixels",
      "Review model changes",
    ],
  },
];
const pixelCreate = [
  "pixel.create",
  { path: pixelPath, width: 4, height: 4, palette: ["#00000000", "#FF0000FF"] },
];
const pixelEdit = [
  "pixel.edit",
  {
    path: pixelPath,
    revision: 1,
    operations: [{ op: "line", from: [0, 0], to: [3, 3], color: 1 }],
  },
];
function sse(turn, call, id = `visual-${turn}`) {
  const base = {
    id: `offline-${turn}`,
    object: "chat.completion.chunk",
    created: 0,
    model: "visual-fixture",
  };
  const delta = call
    ? {
        role: "assistant",
        tool_calls: [
          {
            index: 0,
            id,
            type: "function",
            function: {
              name: call[0].replace(".", "__"),
              arguments: JSON.stringify(call[1]),
            },
          },
        ],
      }
    : { role: "assistant", content: "Visual draft ready for review." };
  return new Response(
    [
      { ...base, choices: [{ index: 0, delta, finish_reason: null }] },
      {
        ...base,
        choices: [
          { index: 0, delta: {}, finish_reason: call ? "tool_calls" : "stop" },
        ],
      },
    ]
      .map((value) => `data: ${JSON.stringify(value)}\n\n`)
      .join("") + "data: [DONE]\n\n",
    { headers: { "content-type": "text/event-stream" } },
  );
}
async function fixture(
  t,
  {
    script = [plan, pixelCreate, pixelEdit],
    supportsImages = true,
    mode = "execute",
    files = {},
    disabledVisual = false,
    legacy = false,
    toolId,
    wrapService,
    transport,
  } = {},
) {
  await fs.mkdir(scratch, { recursive: true });
  const root = await fs.mkdtemp(path.join(scratch, "run-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const selection = await normalizePiSelection({
    provider: "pcl-custom",
    model: "visual-fixture",
    api: "openai-completions",
    baseUrl: "http://127.0.0.1:18080/v1",
    supportsImages,
  });
  const requests = [];
  const provider = new LivePiProvider(
    { enabled: true, getSelection: () => selection, limits: unlimited },
    {
      fetch: async (_url, options) => {
        const request = JSON.parse(options.body);
        requests.push(request);
        return transport
          ? transport(request, options, requests.length)
          : sse(
              requests.length,
              script[requests.length - 1],
              toolId?.(requests.length),
            );
      },
    },
  );
  const tools = new ToolRegistry(),
    catalog = new HarnessToolCatalog(tools);
  t.after(() => catalog.dispose());
  const runtime = await new AgentRuntime({
    root,
    tools,
    providers: [provider],
  }).init();
  t.after(() => runtime.shutdown());
  const harness = new HarnessService({ store: runtime.store }),
    service = new VisualService({
      store: runtime.store,
      harnessService: harness,
    });
  registerHarnessTools(catalog, harness);
  registerVisualTools(
    catalog.forPlugin("rh.pixel"),
    wrapService ? wrapService(service) : service,
    "pixel",
  );
  registerVisualTools(catalog.forPlugin("rh.model"), service, "model");
  const snapshot = captureNativeSnapshot(
    Object.entries(files).map(([path, text]) => ({
      path,
      sha256: sha256(text),
      data: Buffer.from(text).toString("base64"),
    })),
  );
  const owners = captureHarnessToolScope(undefined, mode).enabledPlugins.filter(
    (id) => !disabledVisual || !["rh.pixel", "rh.model"].includes(id),
  );
  const job = await runtime.createJob({
    profile: "harness",
    provider: provider.id,
    input: {
      prompt: "Edit visual assets and inspect each PNG.",
      harness: {
        schemaVersion: 1,
        mode,
        pluginId: "rh.project",
        projectId: "fixture",
        conversationId: "fixture",
        source: snapshot.descriptor,
        ...(legacy ? {} : captureHarnessToolScope(owners, mode)),
      },
    },
  });
  await stageNativeSnapshot(runtime.store, job, snapshot);
  return { runtime, job, root, requests, service, harness, catalog };
}
const images = (request) =>
  request.messages.flatMap((message) =>
    Array.isArray(message.content)
      ? message.content.filter((block) => block.type === "image_url")
      : [],
  );
const resultFor = (request, name) => {
  const call = request.messages
    .filter((message) => message.role === "assistant")
    .flatMap((message) => message.tool_calls ?? [])
    .find((call) => call.function.name === name.replace(".", "__"));
  return request.messages.find(
    (message) => message.role === "tool" && message.tool_call_id === call?.id,
  );
};
test("real SDK attaches genuine PNG image content after visual edits and preserves compact replay receipts", async (t) => {
  const script = [
    plan,
    pixelCreate,
    pixelEdit,
    pixelEdit,
    ["pixel.inspect", { path: pixelPath }],
    ["model.create", { path: modelPath }],
    [
      "model.edit",
      {
        path: modelPath,
        revision: 1,
        operations: [{ op: "update", id: "cube", patch: { to: [8, 16, 8] } }],
      },
    ],
    [
      "model.propose",
      { path: modelPath, revision: 2, summary: "A narrow cuboid" },
    ],
  ];
  const f = await fixture(t, {
    script,
    toolId: (turn) => (turn === 4 ? "visual-3" : `visual-${turn}`),
  });
  const result = await f.runtime.start(f.job.id);
  assert.equal(result.status, "completed", JSON.stringify(result.error));
  assert.equal(f.requests.length, script.length + 1);
  const image = images(f.requests[2])[0];
  assert.ok(image.image_url.url.startsWith("data:image/png;base64,"));
  const encoded = image.image_url.url.slice("data:image/png;base64,".length);
  assert.equal(
    Buffer.from(encoded, "base64").subarray(0, 8).toString("hex"),
    "89504e470d0a1a0a",
  );
  const tool = resultFor(f.requests[2], "pixel.create");
  assert.equal(JSON.parse(tool.content).visualFeedback.status, "available");
  assert.ok(!tool.content.includes(encoded));
  assert.equal(
    (await f.service.preview(f.job.id)).find(
      (artifact) => artifact.kind === "pixel",
    ).revision,
    2,
  );
  assert.equal(
    result.operations.filter((op) => op.name === "pixel.edit").length,
    1,
  );
  const serialized = JSON.stringify(result);
  assert.ok(!serialized.includes(encoded));
  const log = await fs.readFile(
    path.join(f.root, f.job.id, "events.jsonl"),
    "utf8",
  );
  assert.ok(!log.includes(encoded));
  assert.ok(
    result.operations
      .filter((op) => /^(pixel|model)\.(create|edit|inspect)$/.test(op.name))
      .every(
        (op) =>
          !Object.hasOwn(op.result.image, "data") &&
          Buffer.byteLength(JSON.stringify(op.result)) < 1000,
      ),
  );
  const proposal = result.operations.find(
    (op) => op.name === "model.propose",
  ).result;
  const review = await f.harness.getReview(f.job.id, proposal.proposalId);
  assert.equal(review.changes[0].expectedSha256, null);
  assert.equal(review.changes[0].path, modelPath);
  assert.equal((await f.harness.listReviews(f.job.id)).proposals.length, 1);
});
test("nonvision models receive explicit visual-unavailable feedback while the editor still gets a PNG", async (t) => {
  const f = await fixture(t, {
    script: [plan, pixelCreate],
    supportsImages: false,
  });
  const result = await f.runtime.start(f.job.id);
  assert.equal(result.status, "completed", JSON.stringify(result.error));
  assert.equal(images(f.requests[2]).length, 0);
  const content = JSON.parse(resultFor(f.requests[2], "pixel.create").content);
  assert.equal(content.visualFeedback.status, "unavailable");
  assert.equal(content.visualFeedback.reason, "model_does_not_accept_images");
  assert.ok(!Object.hasOwn(content.image, "data"));
  assert.ok((await f.service.preview(f.job.id))[0].image.data.length > 0);
});
test("real tool loop enforces plan before any private visual draft", async (t) => {
  const f = await fixture(t, { script: [pixelCreate] });
  const result = await f.runtime.start(f.job.id);
  assert.equal(result.status, "completed", JSON.stringify(result.error));
  assert.equal(result.operations.length, 0);
  assert.ok(
    f.requests[1].messages.some(
      (message) =>
        message.role === "tool" && message.content.includes("Record a plan"),
    ),
  );
  assert.deepEqual(await f.service.preview(f.job.id), []);
});
test("Ask and Plan expose inspection only; plugin removal and historical grants never widen tool access", async (t) => {
  for (const mode of ["ask", "plan"]) {
    const f = await fixture(t, {
      mode,
      files: { [pixelPath]: JSON.stringify(createPixel(2, 2)) },
      script: [["pixel.inspect", { path: pixelPath }]],
    });
    const result = await f.runtime.start(f.job.id);
    assert.equal(result.status, "completed", JSON.stringify(result.error));
    const names = f.requests[0].tools.map((tool) => tool.function.name);
    assert.ok(names.includes("pixel__inspect"));
    assert.ok(names.includes("model__inspect"));
    assert.ok(
      !names.some((name) =>
        /^(pixel|model)__(create|edit|propose)$/.test(name),
      ),
    );
    assert.deepEqual(await f.service.preview(f.job.id), []);
  }
  for (const option of [{ disabledVisual: true }, { legacy: true }]) {
    const f = await fixture(t, { ...option, script: [] });
    assert.equal((await f.runtime.start(f.job.id)).status, "completed");
    assert.ok(
      !f.requests[0].tools.some((tool) =>
        /^(pixel|model)__/.test(tool.function.name),
      ),
    );
  }
});
test("cancel during a real SDK visual tool aborts without publishing draft bytes or user source", async (t) => {
  let entered;
  const ready = new Promise((resolve) => {
    entered = resolve;
  });
  const f = await fixture(t, {
    script: [plan, pixelCreate],
    wrapService: (service) => ({
      inspect: service.inspect.bind(service),
      edit: service.edit.bind(service),
      propose: service.propose.bind(service),
      async create(ctx) {
        entered();
        await new Promise((resolve) =>
          ctx.signal.addEventListener("abort", resolve, { once: true }),
        );
        throw Object.assign(new Error("cancelled"), { code: "CANCELLED" });
      },
    }),
  });
  const run = f.runtime.start(f.job.id);
  await ready;
  assert.equal(await f.runtime.cancel(f.job.id), true);
  assert.equal((await run).status, "cancelled");
  assert.deepEqual(await f.service.preview(f.job.id), []);
});
test("same-ID inspect pins its committed revision after later edits rather than replaying a new image", async (t) => {
  const inspect = ["pixel.inspect", { path: pixelPath }];
  const f = await fixture(t, {
    script: [plan, pixelCreate, inspect, pixelEdit, inspect],
    toolId: (turn) => (turn === 5 ? "visual-3" : `visual-${turn}`),
  });
  const result = await f.runtime.start(f.job.id);
  assert.equal(result.status, "completed", JSON.stringify(result.error));
  const imagesBefore = images(f.requests[3]),
    imagesAfter = images(f.requests[5]);
  assert.equal(
    imagesAfter.at(-1).image_url.url,
    imagesBefore.at(-1).image_url.url,
  );
  assert.equal(
    result.operations.find((op) => op.name === "pixel.inspect").result.revision,
    1,
  );
});
test("only idempotent tools with receipt projection may reconstruct completed effects", () => {
  const base = {
    name: "example.replay",
    description: "fixture",
    validate() {},
    execute() {},
    replayCompleted: true,
  };
  assert.throws(
    () =>
      new ToolRegistry().register({
        ...base,
        replaySafety: "manual",
        projectReceipt: (x) => x,
      }),
    { code: "INVALID_TOOL" },
  );
  assert.throws(
    () => new ToolRegistry().register({ ...base, replaySafety: "idempotent" }),
    { code: "INVALID_TOOL" },
  );
  assert.doesNotThrow(() =>
    new ToolRegistry().register({
      ...base,
      replaySafety: "idempotent",
      projectReceipt: (x) => x,
    }),
  );
});
