/** Actual factory and Python source snapshots; no provider, jobs or model calls. */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createAgentHost } from "../../agent-host.mjs";
import { ExtensionStore } from "../../extensions/src/store.mjs";

test("source tokens remain reusable until exact explicit release, and durable projects survive", async (t) => {
  const scratch = fileURLToPath(
    new URL("../../../work/source-release-fixtures/", import.meta.url),
  );
  await fs.mkdir(scratch, { recursive: true });
  const base = await fs.mkdtemp(path.join(scratch, "source-"));
  t.after(() => fs.rm(base, { recursive: true, force: true }));
  const root = path.join(base, "private-host"),
    source = path.join(base, "synthetic-source");
  await fs.mkdir(root, { mode: 0o700 });
  await fs.mkdir(source, { mode: 0o700 });
  await fs.writeFile(path.join(source, "Fixture.java"), "class Fixture {}\n");
  const extensions = await ExtensionStore.open({
    file: path.join(root, "extensions.json"),
  });
  const host = await createAgentHost({ root, extensions });
  t.after(() => host.dispatch("shutdown", {}));
  const call = (op, args = {}) => host.dispatch(op, args);
  assert.equal((await call("idle_prepare")).busy, false);
  const first = await call("source_import", { directory: source });
  const second = await call("source_import", { directory: source });
  assert.equal((await call("idle_prepare")).busy, true);
  const create = (name) =>
    call("porter_project_create", {
      name,
      goal: "Offline source retention",
      targetId: "neoforge-26.3",
      rights: "owner",
      acknowledgeBeta: true,
      sourceId: first.sourceId,
      permittedPaths: ["Fixture.java"],
    });
  await assert.rejects(create(""));
  const project = await create("First durable project");
  const another = await create("Reused source project");
  assert.deepEqual(another.snapshot.files, first.files);
  assert.equal((await call("idle_prepare")).busy, true);
  await assert.rejects(
    call("source_release", { sourceId: "invalid" }),
    /Invalid source release/,
  );
  assert.equal(
    await call("source_release", {
      sourceId: "00000000-0000-4000-8000-000000000001",
    }),
    false,
  );
  assert.equal(
    await call("source_release", { sourceId: first.sourceId }),
    true,
  );
  assert.equal(
    await call("source_release", { sourceId: first.sourceId }),
    false,
  );
  assert.equal(
    (await call("idle_prepare")).busy,
    true,
    "second owner still pins the host",
  );
  const file = path.join(root, "porter-projects", project.id, "project.json");
  const before = await fs.readFile(file);
  assert.deepEqual(
    (await call("porter_project_read", { projectId: project.id })).snapshot,
    project.snapshot,
  );
  assert.equal(
    await call("source_release", { sourceId: second.sourceId }),
    true,
  );
  assert.equal((await call("idle_prepare")).busy, false);
  assert.deepEqual(await fs.readFile(file), before);
  assert.deepEqual(
    (await call("porter_project_read", { projectId: another.id })).snapshot
      .files,
    first.files,
  );
});
