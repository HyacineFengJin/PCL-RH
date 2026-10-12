/** Exercise native-facing stdio with an isolated host and loopback-only fake
 * model. No paid endpoint, user key, domain tool or generated build is used. */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import http from "node:http";
import readline from "node:readline";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
const hostPath = fileURLToPath(new URL("../../host.mjs", import.meta.url));
const scratch = fileURLToPath(
  new URL("../../../work/ai-management-2026-10-07/fixtures/", import.meta.url),
);
const pause = () => new Promise((resolve) => setTimeout(resolve, 10));
async function until(check, label = "host check", timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const result = await check();
    if (result) return result;
    await pause();
  }
  throw new Error(`Offline ${typeof label === "function" ? label() : label} timed out`);
}
function response(res) {
  const chunks = [
    {
      id: "offline",
      object: "chat.completion.chunk",
      created: 0,
      model: "offline",
      choices: [
        {
          index: 0,
          delta: { role: "assistant", content: "Offline preset response." },
          finish_reason: null,
        },
      ],
    },
    {
      id: "offline",
      object: "chat.completion.chunk",
      created: 0,
      model: "offline",
      choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
    },
  ];
  res.writeHead(200, { "content-type": "text/event-stream" });
  res.end(
    chunks.map((chunk) => "data: " + JSON.stringify(chunk) + "\n\n").join("") +
      "data: [DONE]\n\n",
  );
}
test(
  "tools auto-start saved presets; edits do not change active jobs; stop cancels them",
  { timeout: 15000 },
  async (t) => {
    await fs.mkdir(scratch, { recursive: true });
    const root = await fs.mkdtemp(path.join(scratch, "host-"));
    const requests = [];
    const server = http.createServer(async (req, res) => {
      let text = "";
      for await (const chunk of req) text += chunk;
      requests.push({
        path: req.url,
        authorization: req.headers.authorization,
        body: JSON.parse(text),
      });
      if (requests.length !== 1) response(res); // First request stays open until Stop aborts it.
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    const child = spawn(process.execPath, [hostPath, root], {
      stdio: ["pipe", "pipe", "pipe"],
    });
    child.stderr.resume(); // Private provider details must not enter test output.
    const lines = readline.createInterface({ input: child.stdout });
    const replies = [];
    lines.on("line", (line) => replies.push(JSON.parse(line)));
    t.after(async () => {
      child.kill("SIGKILL");
      lines.close();
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
      await fs.rm(root, { recursive: true, force: true });
    });
    async function rpc(operation, args = {}, timeoutMs = 5000) {
      child.stdin.write(JSON.stringify({ operation, args }) + "\n");
      const reply = await until(() => replies.shift(), `${operation} reply`, timeoutMs);
      assert.equal(reply.ok, true, reply.error);
      return reply.result;
    }
    const firstKey = "offline-first-key",
      secondKey = "offline-second-key";
    const config = {
      provider: "pcl-custom",
      model: "offline",
      api: "openai-completions",
      baseUrl: `http://127.0.0.1:${server.address().port}/v1`,
      key: firstKey,
    };
    const saved = await rpc("provider_preset_save", {
      name: "Offline",
      config,
      limits: {
        modelTurns: 0,
        toolCalls: 0,
        maximumOutputTokens: 0,
        wallTimeMs: 0,
        estimatedBudgetUsd: 0,
      },
    });
    assert.equal(JSON.stringify(saved).includes(firstKey), false);
    assert.equal(saved.presets[0].limits.wallTimeMs, 0);
    // Provider selection reads the pinned offline catalog. Finish cold SDK
    // setup before testing the separately bounded transport and Stop behavior.
    const catalog = await rpc("provider_catalog", {}, 10000);
    assert.ok(Array.isArray(catalog.providers));
    const before = await rpc("status");
    assert.equal(before.liveAvailable, true);
    assert.equal(before.liveConfigured, false);
    const first = await rpc("job_create", {
      mode: "live",
      profile: "maker",
      spec: {},
      prompt: "Offline check only",
    });
    let firstStatus = "unknown";
    let firstActivity = null;
    const transportStarted = Date.now(), startupPhases = [];
    await until(async () => {
      if (requests.length === 1) return true;
      const view = await rpc("job_read", { jobId: first.jobId });
      firstStatus = view.summary.status;
      if (["failed", "cancelled", "interrupted", "completed"].includes(firstStatus))
        throw new Error(`Offline job ended before its first transport request (${firstStatus}): ${JSON.stringify(view.error ?? null)}`);
      // This isolated fixture may remain running before transport entry. Keep
      // only bounded host activity metadata in the timeout diagnosis.
      const stored = JSON.parse(await fs.readFile(path.join(root, "jobs", first.jobId, "job.json"), "utf8"));
      firstActivity = { activity: stored.providerState?.livePi?.activity ?? null,
        modelTurns: stored.providerState?.livePi?.modelTurns ?? null, requests: stored.providerState?.livePi?.requests ?? null };
      const phase = firstActivity.activity?.phase ?? "not_checkpointed";
      if (startupPhases.at(-1)?.phase !== phase && startupPhases.length < 8)
        startupPhases.push({ elapsedMs: Date.now() - transportStarted, phase });
      return false;
    }, () => `first transport request (last job status: ${firstStatus}, activity: ${JSON.stringify(firstActivity)}, startup phases: ${JSON.stringify(startupPhases)})`);
    assert.equal((await rpc("status")).liveConfigured, true);
    await rpc("provider_preset_save", {
      id: saved.selectedId,
      name: "Edited",
      config: { ...config, key: secondKey },
      limits: { maximumOutputTokens: 8192 },
    });
    const second = await rpc("job_create", {
      mode: "live",
      profile: "maker",
      spec: {},
      prompt: "Another offline check",
    });
    const finished = await until(async () => {
      const view = await rpc("job_read", { jobId: second.jobId });
      if (["failed", "cancelled", "interrupted"].includes(view.summary.status))
        throw new Error(`Offline second job ended before completion (${view.summary.status}): ${JSON.stringify(view.error ?? null)}`);
      return view.summary.status === "completed" && view;
    }, "second job completion");
    assert.equal(finished.result.text, "Offline preset response.");
    // The SDK also reserves room for input in the model's context window.
    assert.ok(
      requests[0].body.max_tokens > 16384 &&
        requests[0].body.max_tokens <= 32768,
    );
    assert.equal(requests[0].authorization, "Bearer " + firstKey);
    assert.equal(requests[1].body.max_tokens, 8192);
    assert.equal(requests[1].authorization, "Bearer " + secondKey);
    assert.equal(
      requests.every((r) => r.path === "/v1/chat/completions"),
      true,
    );
    const stopped = await rpc("provider_stop");
    assert.equal(stopped.liveConfigured, false);
    const cancelled = await rpc("job_read", { jobId: first.jobId });
    assert.equal(cancelled.summary.status, "cancelled");
    assert.equal(JSON.stringify(cancelled).includes(firstKey), false);
    const after = await rpc("status");
    assert.equal(after.liveAvailable, true);
    assert.equal(after.aiPresets.presets.length, 1);
    const jobs = await fs.readdir(path.join(root, "jobs"));
    for (const id of jobs.filter((id) => /^[a-f0-9-]{36}$/.test(id))) {
      const bytes = await fs.readFile(
        path.join(root, "jobs", id, "job.json"),
        "utf8",
      );
      assert.equal(
        bytes.includes(firstKey) || bytes.includes(secondKey),
        false,
      );
    }
    await rpc("shutdown");
  },
);
