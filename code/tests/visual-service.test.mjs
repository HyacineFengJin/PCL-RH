import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { JobStore } from "../../experimental/runtime/src/store.mjs";
import { HarnessService } from "../harness-service.mjs";
import { VisualService, VISUAL_LIMITS } from "../visual-service.mjs";
import { createPixel, createModel } from "../visual-assets.mjs";
import {
  captureNativeSnapshot,
  stageNativeSnapshot,
  sha256,
} from "../harness-snapshot.mjs";

const scratch = fileURLToPath(
  new URL(
    "../../work/visual-assets-2026-10-13/service-tests/",
    import.meta.url,
  ),
);
async function fixture(
  t,
  { mode = "execute", plan = true, files = {}, protectedPaths = [] } = {},
) {
  await fs.mkdir(scratch, { recursive: true });
  const root = await fs.mkdtemp(path.join(scratch, "run-"));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const store = await new JobStore(root).init();
  const snapshot = captureNativeSnapshot(
    Object.entries(files).map(([path, text]) => ({
      path,
      data: Buffer.from(text).toString("base64"),
      sha256: sha256(text),
    })),
    protectedPaths,
  );
  const job = await store.create({
    profile: "harness",
    provider: "offline",
    input: {
      harness: {
        schemaVersion: 1,
        mode,
        projectId: "fixture",
        conversationId: "fixture",
        source: snapshot.descriptor,
      },
    },
    operations: plan
      ? [{ id: "plan", name: "harness.report_plan", status: "completed" }]
      : [],
  });
  await stageNativeSnapshot(store, job, snapshot);
  const harness = new HarnessService({ store }),
    service = new VisualService({ store, harnessService: harness });
  const ctx = {
    jobId: job.id,
    operationId: "create",
    signal: new AbortController().signal,
  };
  return { root, store, snapshot, job, harness, service, ctx };
}
const pixelPath = "textures/example.rhtexture.json";
const modelPath = "models/example.rhmodel.json";
test("pixel drafts render real bounded PNGs, replay deterministically and retain source hashes through review", async (t) => {
  const original = JSON.stringify(createPixel(4, 4));
  const f = await fixture(t, { files: { [pixelPath]: original } });
  const originalInspect = await f.service.inspect(f.ctx, "pixel", {
    path: pixelPath,
    region: { x: 0, y: 0, width: 2, height: 2 },
  });
  assert.equal(originalInspect.revision, 0);
  assert.equal(originalInspect.expectedSha256, sha256(original));
  assert.deepEqual(originalInspect.summary.region.pixels, [
    [0, 0],
    [0, 0],
  ]);
  const edit = {
    path: pixelPath,
    revision: 0,
    operations: [
      { op: "palette", index: 1, color: "#FF0000FF" },
      { op: "line", from: [0, 0], to: [3, 3], color: 1 },
    ],
  };
  const first = await f.service.edit(
    { ...f.ctx, operationId: "edit" },
    "pixel",
    edit,
  );
  assert.equal(first.revision, 1);
  assert.notEqual(first.image.sha256, originalInspect.image.sha256);
  assert.equal(
    Buffer.from(first.image.data, "base64").subarray(0, 8).toString("hex"),
    "89504e470d0a1a0a",
  );
  assert.ok(
    Buffer.byteLength(JSON.stringify(first)) <= VISUAL_LIMITS.resultBytes,
  );
  assert.deepEqual(
    await f.service.edit({ ...f.ctx, operationId: "edit" }, "pixel", edit),
    first,
  );
  await assert.rejects(
    f.service.edit({ ...f.ctx, operationId: "edit" }, "pixel", {
      ...edit,
      revision: 1,
    }),
    { code: "OPERATION_MISMATCH" },
  );
  await assert.rejects(
    f.service.edit({ ...f.ctx, operationId: "stale" }, "pixel", edit),
    { code: "VISUAL_REVISION_CONFLICT" },
  );
  const proposal = await f.service.propose(
    { ...f.ctx, operationId: "propose" },
    "pixel",
    { path: pixelPath, revision: 1, summary: "Draw a red diagonal" },
  );
  const review = await f.harness.getReview(f.job.id, proposal.proposalId);
  assert.equal(review.changes[0].expectedSha256, sha256(original));
  assert.equal(review.changes[0].path, pixelPath);
  assert.equal(proposal.originalSourceModified, false);
  assert.equal(
    (await (await f.store.workspace(f.job.id)).read(`source/${pixelPath}`))
      .content,
    original,
  );
});
test("new model revisions preserve cuboids, rotations and texture edits with reproducible previews", async (t) => {
  const f = await fixture(t);
  const first = await f.service.create(f.ctx, "model", { path: modelPath });
  const edited = await f.service.edit(
    { ...f.ctx, operationId: "edit-model" },
    "model",
    {
      path: modelPath,
      revision: 1,
      operations: [
        {
          op: "update",
          id: "cube",
          patch: {
            to: [10, 16, 10],
            rotation: { origin: [8, 8, 8], axis: "y", angle: 22.5 },
          },
        },
        {
          op: "texture",
          operations: [{ op: "palette", index: 1, color: "#33AA55FF" }],
        },
      ],
    },
  );
  assert.equal(edited.revision, 2);
  assert.notEqual(edited.image.sha256, first.image.sha256);
  const preview = await f.service.preview(f.job.id);
  assert.equal(preview[0].document.elements[0].rotation.angle, 22.5);
  assert.equal(preview[0].document.texture.palette[1], "#33AA55FF");
  assert.equal(preview[0].image.sha256, edited.image.sha256);
  const inspected = await f.service.inspect(f.ctx, "model", {
    path: modelPath,
    revision: 1,
  });
  assert.equal(inspected.image.sha256, first.image.sha256);
  const underside = await f.service.inspect(f.ctx, "model", {
    path: modelPath,
    revision: 2,
    view: { yaw: 225, pitch: -30 },
  });
  assert.notEqual(underside.image.sha256, edited.image.sha256);
  assert.deepEqual(underside.summary.view, { yaw: 225, pitch: -30 });
});
test("Ask and Plan inspect frozen visuals while all draft writes require Execute and a recorded plan", async (t) => {
  for (const mode of ["ask", "plan"]) {
    const f = await fixture(t, {
      mode,
      files: { [modelPath]: JSON.stringify(createModel()) },
    });
    assert.equal(
      (await f.service.inspect(f.ctx, "model", { path: modelPath })).revision,
      0,
    );
    await assert.rejects(
      f.service.create(f.ctx, "pixel", { path: pixelPath }),
      { code: "TOOL_DENIED" },
    );
    await assert.rejects(
      f.service.edit(f.ctx, "model", {
        path: modelPath,
        revision: 0,
        operations: [{ op: "remove", id: "cube" }],
      }),
      { code: "TOOL_DENIED" },
    );
    assert.deepEqual(await f.service.preview(f.job.id), []);
  }
  const f = await fixture(t, { plan: false });
  await assert.rejects(f.service.create(f.ctx, "pixel", { path: pixelPath }), {
    code: "PLAN_REQUIRED",
  });
  assert.deepEqual(await f.service.preview(f.job.id), []);
});
test("invalid batches, protected originals and source tampering fail before publishing another revision", async (t) => {
  const original = JSON.stringify(createPixel(2, 2));
  const f = await fixture(t, {
    files: { [pixelPath]: original },
    protectedPaths: [pixelPath],
  });
  await assert.rejects(
    f.service.edit(f.ctx, "pixel", {
      path: pixelPath,
      revision: 0,
      operations: [{ op: "fill", x: 0, y: 0, color: 0 }],
    }),
    { code: "HANDWRITTEN_PROTECTED" },
  );
  const g = await fixture(t);
  await g.service.create(g.ctx, "pixel", {
    path: pixelPath,
    width: 2,
    height: 2,
  });
  await assert.rejects(
    g.service.edit({ ...g.ctx, operationId: "invalid" }, "pixel", {
      path: pixelPath,
      revision: 1,
      operations: [
        { op: "palette", index: 1, color: "#FF0000FF" },
        { op: "brush", points: [[2, 0]], color: 1 },
      ],
    }),
    { code: "INVALID_VISUAL_ASSET" },
  );
  assert.equal((await g.service.preview(g.job.id))[0].revision, 1);
  const h = await fixture(t, { files: { [pixelPath]: original } });
  const edit = {
    path: pixelPath,
    revision: 0,
    operations: [{ op: "palette", index: 1, color: "#FF0000FF" }],
  };
  await h.service.edit(h.ctx, "pixel", edit);
  await fs.writeFile(
    await (await h.store.workspace(h.job.id)).resolve(`source/${pixelPath}`),
    original.replace('"width":2', '"width":3'),
  );
  await assert.rejects(h.service.edit(h.ctx, "pixel", edit), {
    code: "SOURCE_SNAPSHOT_CHANGED",
  });
});
test("cancelled staging is invisible and a different operation can create without colliding with orphan bytes", async (t) => {
  const f = await fixture(t),
    controller = new AbortController();
  const originalFiles = f.store.files.bind(f.store);
  f.store.files = async (id) => {
    const control = await originalFiles(id),
      resolve = control.resolve.bind(control);
    control.resolve = async (...args) => {
      const result = await resolve(...args);
      if (
        args[0].startsWith("control/visual-assets/") &&
        /\/[1-9][0-9]*-[a-f0-9]{64}\.json$/.test(args[0])
      )
        controller.abort();
      return result;
    };
    return control;
  };
  await assert.rejects(
    f.service.create({ ...f.ctx, signal: controller.signal }, "pixel", {
      path: pixelPath,
      width: 2,
      height: 2,
    }),
    { code: "CANCELLED" },
  );
  f.store.files = originalFiles;
  assert.deepEqual(await f.service.preview(f.job.id), []);
  const created = await f.service.create(
    { ...f.ctx, operationId: "replacement" },
    "pixel",
    { path: pixelPath, width: 3, height: 3 },
  );
  assert.equal(created.revision, 1);
  assert.equal(created.summary.width, 3);
});
test("public previews retain only two latest assets and bound noisy maximum textures", async (t) => {
  const f = await fixture(t);
  const palette = [
    "#00000000",
    ...Array.from(
      { length: 255 },
      (_, i) =>
        `#${((i * 65891) % 16777216).toString(16).padStart(6, "0").toUpperCase()}FF`,
    ),
  ];
  for (let i = 0; i < 3; i++)
    await f.service.create({ ...f.ctx, operationId: `new-${i}` }, "pixel", {
      path: `textures/${i}.rhtexture.json`,
      width: 128,
      height: 128,
      palette,
    });
  const groups = Array.from({ length: 255 }, () => []);
  let random = 42;
  for (let y = 0; y < 128; y++)
    for (let x = 0; x < 128; x++) {
      random = (Math.imul(random, 1664525) + 1013904223) >>> 0;
      groups[random % 255].push([x, y]);
    }
  const noisy = await f.service.edit(
    { ...f.ctx, operationId: "noise" },
    "pixel",
    {
      path: "textures/2.rhtexture.json",
      revision: 1,
      operations: groups.map((points, index) => ({
        op: "brush",
        points,
        color: index + 1,
      })),
    },
  );
  assert.ok(noisy.image.bytes <= VISUAL_LIMITS.imageBytes);
  const preview = await f.service.preview(f.job.id);
  assert.equal(preview.length, 2);
  assert.equal(preview[0].path, "textures/2.rhtexture.json");
  assert.ok(Buffer.byteLength(JSON.stringify(preview)) <= 160_000);
});
test("draft revision corruption refuses inspection and preview instead of serving a cached image", async (t) => {
  const f = await fixture(t);
  await f.service.create(f.ctx, "pixel", { path: pixelPath });
  const folder = path.join(
    f.root,
    f.job.id,
    "control/visual-assets",
    sha256(pixelPath),
  );
  const target = path.join(folder, (await fs.readdir(folder))[0]);
  const record = JSON.parse(await fs.readFile(target, "utf8"));
  record.document.pixels[0] = 1;
  await fs.writeFile(target, JSON.stringify(record));
  await assert.rejects(f.service.inspect(f.ctx, "pixel", { path: pixelPath }), {
    code: "VISUAL_DRAFT_CHANGED",
  });
  await assert.rejects(f.service.preview(f.job.id), {
    code: "VISUAL_DRAFT_CHANGED",
  });
});
