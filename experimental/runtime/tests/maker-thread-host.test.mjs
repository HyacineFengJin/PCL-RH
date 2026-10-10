/** Native-facing thread IPC and the pinned SDK against loopback only. No paid
 * endpoint, real preset, source generation or game is accessed. */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import http from "node:http";
import readline from "node:readline";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
const host = fileURLToPath(new URL("../../host.mjs", import.meta.url)),
  scratch = fileURLToPath(
    new URL("../../../work/maker-agent/host-test/", import.meta.url),
  );
async function until(check) {
  const end = Date.now() + 6000;
  while (Date.now() < end) {
    const value = await check();
    if (value) return value;
    await new Promise((r) => setTimeout(r, 10));
  }
  throw new Error("Offline host timeout");
}
test(
  "thread modes, captured charter and history survive a host restart without exposing a key",
  { timeout: 20000 },
  async (t) => {
    await fs.mkdir(scratch, { recursive: true });
    const root = await fs.mkdtemp(path.join(scratch, "ipc-")),
      requests = [];
    const server = http.createServer(async (req, res) => {
      let text = "";
      for await (const chunk of req) text += chunk;
      requests.push(JSON.parse(text));
      const delta = {
        id: "offline",
        object: "chat.completion.chunk",
        created: 0,
        model: "offline",
        choices: [
          {
            index: 0,
            delta: { role: "assistant", content: "Offline thread reply" },
            finish_reason: null,
          },
        ],
      };
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.end(
        "data: " +
          JSON.stringify(delta) +
          "\n\ndata: " +
          JSON.stringify({
            ...delta,
            choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
          }) +
          "\n\ndata: [DONE]\n\n",
      );
    });
    await new Promise((r) => server.listen(0, "127.0.0.1", r));
    let child, lines, replies;
    function open() {
      child = spawn(process.execPath, [host, root], {
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
      await fs.rm(root, { recursive: true, force: true });
    });
    open();
    const key = "isolated-loopback-only";
    await rpc("provider_preset_save", {
      name: "Offline",
      config: {
        provider: "pcl-custom",
        model: "offline",
        api: "openai-completions",
        baseUrl: `http://127.0.0.1:${server.address().port}/v1`,
        key,
      },
    });
    const first = await rpc("maker_thread_run", {
      name: "Example",
      notes: "Preserve combat balance",
      prompt: "Explain the constraints",
      request: "Explain the constraints",
      mode: "ask",
    });
    await until(async () => {
      const j = await rpc("job_read", { jobId: first.rounds[0].jobId });
      return j.summary.status === "completed";
    });
    const second = await rpc("maker_thread_run", {
      threadId: first.id,
      prompt: "Plan the next increment",
      mode: "plan",
    });
    await until(async () => {
      const j = await rpc("job_read", { jobId: second.rounds[1].jobId });
      return j.summary.status === "completed";
    });
    for (const request of requests) {
      const names = request.tools.map((tool) => tool.function.name);
      assert.ok(!names.includes("workspace__write"));
      assert.ok(!names.includes("maker__generate"));
      assert.ok(names.includes("maker__report_plan"));
      assert.ok(
        JSON.stringify(request.messages).includes("Preserve combat balance"),
      );
    }
    assert.ok(
      JSON.stringify(requests[1].messages).includes("Offline thread reply"),
    );
    // RH Code must prepare without contacting even the loopback provider. The
    // native store binds this ID before the separate start route is admitted.
    const code = await rpc("rh_code_run", {
      project: { id: "fixture-project", name: "RH fixture" },
      conversation: {
        id: "fixture-conversation",
        messages: [{ role: "assistant", content: "Prior RH context" }],
      },
      request: "Explain this project",
      mode: "ask",
      target: { minecraft: "1.21.1", loaders: ["Fabric", "NeoForge"] },
      sourceFiles: [
        {
          path: "notes.txt",
          data: Buffer.alloc(160000, 65).toString("base64"),
          sha256: createHash("sha256")
            .update(Buffer.alloc(160000, 65))
            .digest("hex"),
        },
      ],
      source: {},
    });
    assert.equal(
      (await rpc("job_read", { jobId: code.jobId })).summary.status,
      "queued",
    );
    assert.equal(requests.length, 2);
    await assert.rejects(
      rpc("rh_code_start", {
        jobId: code.jobId,
        projectId: "another-project",
        conversationId: "fixture-conversation",
      }),
      /belongs to another/,
    );
    await assert.rejects(
      rpc("rh_code_artifact", {
        jobId: code.jobId,
        projectId: "another-project",
      }),
      /belongs to another/,
    );
    await rpc("rh_code_start", {
      jobId: code.jobId,
      projectId: "fixture-project",
      conversationId: "fixture-conversation",
    });
    await until(
      async () =>
        (await rpc("job_read", { jobId: code.jobId })).summary.status ===
        "completed",
    );
    assert.match(JSON.stringify(requests[2].messages), /RH fixture/);
    assert.match(JSON.stringify(requests[2].messages), /Prior RH context/);
    assert.ok(
      !requests[2].tools.some(
        (tool) => tool.function.name === "maker__generate",
      ),
    );
    const listing = await rpc("maker_threads");
    assert.equal(listing.threads.length, 1);
    assert.equal(listing.threads[0].roundCount, 2);
    assert.equal(listing.threads[0].rounds.length, 1);
    assert.ok(!JSON.stringify(listing).includes(key));
    const exited = new Promise((r) => child.once("exit", r));
    await rpc("shutdown");
    child.stdin.end();
    await exited;
    lines.close();
    open();
    const restored = await rpc("maker_thread_read", { threadId: first.id });
    assert.equal(restored.rounds.length, 2);
    assert.equal(restored.rounds[0].mode, "ask");
    assert.equal(restored.rounds[1].mode, "plan");
    assert.equal(restored.notes, "Preserve combat balance");
    assert.ok(!JSON.stringify(restored).includes(key));
  },
);
