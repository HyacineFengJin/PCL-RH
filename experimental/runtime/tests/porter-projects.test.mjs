import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import { PorterProjects } from "../../porter-projects.mjs";
import {
  parsePorterOrigin,
  resolvePorterOrigin,
} from "../../porter-origins.mjs";
const scratch = fileURLToPath(
  new URL("../../../work/porter-tests/projects/", import.meta.url),
);
const source = {
  files: { "src/main/java/example/Example.java": "class Example {}\n" },
  permittedPaths: ["src/main/java/example/Example.java"],
};
async function fixture(t) {
  await fs.mkdir(scratch, { recursive: true });
  const root = await fs.mkdtemp(path.join(scratch, "projects-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const jobs = new Map(),
    active = new Set();
  const runtime = {
    getJob: async (id) => {
      if (!jobs.has(id)) throw new Error("missing job");
      return jobs.get(id);
    },
    hasActiveJob: (id) => active.has(id),
  };
  const projects = await new PorterProjects(root, runtime).init();
  const p = await projects.create({
    name: "Example migration",
    goal: "Preserve behavior",
    targetId: "neoforge-26.2",
    rights: "owner",
    source,
  });
  const createJob = async (input) => {
    const job = { id: randomUUID(), input, status: "running", result: null };
    jobs.set(job.id, job);
    active.add(job.id);
    return job;
  };
  return { root, projects, p, jobs, active, runtime, createJob };
}

test("discussion, questions and source baselines persist across rounds and restart", async (t) => {
  const { root, projects, p, jobs, active, runtime, createJob } =
    await fixture(t);
  let current = await projects.addMessage(p.id, {
    revision: p.revision,
    content: "Keep the existing item",
    kind: "request",
  });
  const round = await projects.beginRound(
    p.id,
    { revision: current.revision, mode: "live" },
    createJob,
  );
  const job = jobs.get(round.jobId);
  assert.equal(job.input.conversation[0].content, "Keep the existing item");
  const question = await projects.askUser(job.id, {
    question: "Client only or server too?",
    options: ["Both", "Client only"],
  });
  await projects.reportProgress(job.id, {
    summary: "Entry points need review",
    completed: [],
    remaining: ["Server path"],
    limitations: ["Not built"],
  });
  job.status = "completed";
  job.result = { label: "PORTER_AWAITING_USER", text: question.text };
  active.delete(job.id);
  current = await projects.read(p.id);
  assert.equal(current.activeRound, null);
  assert.equal(current.awaitingQuestion, question.questionId);
  assert.equal(current.messages.filter((m) => m.kind === "question").length, 1);
  await assert.rejects(
    projects.beginRound(
      p.id,
      { revision: current.revision, mode: "live" },
      createJob,
    ),
    /answer pending questions/,
  );
  current = await projects.addMessage(p.id, {
    revision: current.revision,
    kind: "answer",
    content: "Both",
    replyTo: question.questionId,
  });
  const next = await projects.beginRound(
    p.id,
    { revision: current.revision, mode: "live" },
    createJob,
  );
  assert.ok(
    jobs.get(next.jobId).input.conversation.some((m) => m.content === "Both"),
  );
  const reloaded = await new PorterProjects(root, runtime).init();
  assert.equal((await reloaded.read(p.id)).rounds.length, 2);
  assert.equal(
    (await reloaded.read(p.id)).snapshot.files[source.permittedPaths[0]],
    source.files[source.permittedPaths[0]],
  );
});

test("feedback during a running round is saved for the next frozen run", async (t) => {
  const { projects, p, jobs, active, createJob } = await fixture(t);
  const round = await projects.beginRound(
    p.id,
    { revision: p.revision, mode: "live" },
    createJob,
  );
  const current = await projects.read(p.id);
  const updated = await projects.addMessage(p.id, {
    revision: current.revision,
    content: "Server crash",
    kind: "bug",
  });
  assert.equal(jobs.get(round.jobId).input.conversation.length, 0);
  await assert.rejects(
    projects.setSource(p.id, { revision: updated.revision, source }),
    /finish the current round/,
  );
  jobs.get(round.jobId).status = "completed";
  active.delete(round.jobId);
  const after = await projects.read(p.id);
  assert.equal(after.messages[0].content, "Server crash");
  const next = await projects.beginRound(
    p.id,
    { revision: after.revision, mode: "template" },
    createJob,
  );
  assert.ok(
    jobs.get(next.jobId).input.conversation.some((m) => m.kind === "bug"),
  );
});

test("stale writes and unsupported records are refused while history is retained", async (t) => {
  const { root, projects, p } = await fixture(t);
  await projects.addMessage(p.id, { revision: p.revision, content: "First" });
  await assert.rejects(
    projects.addMessage(p.id, { revision: p.revision, content: "Lost update" }),
    /reload/,
  );
  const file = path.join(root, "porter-projects", p.id, "project.json");
  const data = JSON.parse(await fs.readFile(file, "utf8"));
  data.schemaVersion = 999;
  await fs.writeFile(file, JSON.stringify(data), { mode: 0o600 });
  const bytes = await fs.readFile(file);
  assert.deepEqual((await projects.list()).unreadableProjectIds, [p.id]);
  await assert.rejects(
    projects.addMessage(p.id, { revision: 2, content: "Overwrite" }),
  );
  assert.deepEqual(await fs.readFile(file), bytes);
  data.schemaVersion = 1;
  data.name = "External project edit";
  await fs.writeFile(file, JSON.stringify(data), { mode: 0o600 });
  assert.deepEqual((await projects.list()).unreadableProjectIds, [p.id]);
  const reloaded = await new PorterProjects(root, {}).init();
  assert.equal((await reloaded.read(p.id)).messages[0].content, "First");
  data.snapshot.revision = "damaged";
  await fs.writeFile(file, JSON.stringify(data), { mode: 0o600 });
  const corrupted = await fs.readFile(file);
  const afterRestart = await new PorterProjects(root, {}).init();
  assert.deepEqual((await afterRestart.list()).unreadableProjectIds, [p.id]);
  await assert.rejects(
    afterRestart.addMessage(p.id, {
      revision: 2,
      content: "Overwrite damaged baseline",
    }),
  );
  assert.deepEqual(await fs.readFile(file), corrupted);
});

test("exact generation witness rejects external whitespace and accepts owned durable writes", async (t) => {
  const { root, projects, p } = await fixture(t);
  const updated = await projects.addMessage(p.id, {
    revision: p.revision,
    content: "Owned update",
  });
  assert.equal((await projects.read(p.id)).revision, updated.revision);
  const file = path.join(root, "porter-projects", p.id, "project.json");
  const original = await fs.readFile(file, "utf8");
  const spaced = original + "\n";
  assert.deepEqual(JSON.parse(spaced), JSON.parse(original));
  await fs.writeFile(file, spaced, { mode: 0o600 });
  await assert.rejects(projects.read(p.id), /changed externally/);
  await assert.rejects(
    projects.addMessage(p.id, {
      revision: updated.revision,
      content: "Refused",
    }),
    /changed externally/,
  );
  assert.equal(await fs.readFile(file, "utf8"), spaced);
  await fs.writeFile(file, original, { mode: 0o600 });
  const next = await projects.addMessage(p.id, {
    revision: updated.revision,
    content: "Recovered original generation",
  });
  assert.equal(next.revision, updated.revision + 1);
  assert.equal((await projects.read(p.id)).messages.length, 2);
});

test("interrupted rounds release project admission without deleting old job records", async (t) => {
  const { projects, p, active, jobs, createJob } = await fixture(t);
  const round = await projects.beginRound(
    p.id,
    { revision: p.revision, mode: "live" },
    createJob,
  );
  active.delete(round.jobId);
  const recovered = await projects.read(p.id);
  assert.equal(recovered.activeRound, null);
  assert.equal(recovered.rounds[0].status, "interrupted");
  assert.equal(jobs.get(round.jobId).status, "running");
  const archived = await projects.archive(p.id, {
    revision: recovered.revision,
    archived: true,
  });
  await assert.rejects(
    projects.beginRound(
      p.id,
      { revision: archived.revision, mode: "live" },
      createJob,
    ),
  );
});

test("context retains open requests while omitted old text remains in history", async (t) => {
  const { projects, p, jobs, active, createJob } = await fixture(t);
  let current = await projects.addMessage(p.id, {
    revision: p.revision,
    content: "Keep this requirement",
    kind: "request",
  });
  const first = await projects.beginRound(
    p.id,
    { revision: current.revision, mode: "live" },
    createJob,
  );
  jobs.get(first.jobId).status = "completed";
  jobs.get(first.jobId).result = { text: "a".repeat(18000) };
  active.delete(first.jobId);
  current = await projects.read(p.id);
  current = await projects.addMessage(p.id, {
    revision: current.revision,
    content: "User test log: " + "b".repeat(7000),
    kind: "feedback",
  });
  const second = await projects.beginRound(
    p.id,
    { revision: current.revision, mode: "live" },
    createJob,
  );
  const input = jobs.get(second.jobId).input;
  assert.ok(
    input.conversation.some((m) => m.content === "Keep this requirement"),
  );
  assert.equal(input.discussionContext.omittedMessages, 1);
  assert.equal((await projects.read(p.id)).messages.length, 3);
});

test("supported links resolve identity without downloading or executing source", async () => {
  assert.equal(
    parsePorterOrigin(
      "https://www.curseforge.com/minecraft/mc-mods/example/files/1234",
    ).versionId,
    "1234",
  );
  assert.equal(
    parsePorterOrigin("https://www.mcmod.cn/class/123.html").provider,
    "mcmod",
  );
  for (const url of [
    "http://modrinth.com/mod/example",
    "https://modrinth.com.evil/mod/example",
    "https://modrinth.com/mod/example?key=secret",
    "https://user:secret@modrinth.com/mod/example",
    "https://modrinth.com/modpack/example",
    "https://modrinth.com/mod/%2e%2e",
  ])
    assert.throws(() => parsePorterOrigin(url));
  let requests = 0;
  const resolved = await resolvePorterOrigin(
    "https://modrinth.com/mod/example",
    async (url, options) => {
      requests++;
      assert.equal(url, "https://api.modrinth.com/v2/project/example");
      assert.equal(options.redirect, "error");
      return new Response(
        JSON.stringify({
          id: "Ab12Cd34",
          slug: "example",
          project_type: "mod",
          title: "Example",
          source_url: "https://github.com/example/project",
          license: { id: "MIT" },
        }),
      );
    },
  );
  assert.equal(requests, 1);
  assert.equal(resolved.projectId, "Ab12Cd34");
  assert.equal(resolved.sourceUrl, "https://github.com/example/project");
  const metadata = { id: "Ab12Cd34", slug: "example", project_type: "mod" };
  const resolveVersion = (projectId) =>
    resolvePorterOrigin(
      "https://modrinth.com/mod/example/version/Ve12Rs34",
      async (url) =>
        new Response(
          JSON.stringify(
            url.includes("/project/")
              ? metadata
              : { id: "Ve12Rs34", project_id: projectId },
          ),
        ),
    );
  assert.equal((await resolveVersion("Ab12Cd34")).versionId, "Ve12Rs34");
  await assert.rejects(resolveVersion("OtherMod"), /another project/);
  const reference = await resolvePorterOrigin(
    "https://www.mcmod.cn/class/123.html",
    () => {
      throw new Error("Should not fetch");
    },
  );
  assert.equal(reference.resolution, "reference-only");
});
