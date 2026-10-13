/** Job-owned visual drafts. Immutable revisions are written before an operation
 * commit; enumerating those commits recovers a crash without mutable pointers.
 * Images are reconstructed from verified documents, never stored in transcripts
 * or operation receipts. Publication uses the existing Harness review service.
 */
import fs from "node:fs/promises";
import path from "node:path";
import { check, throwIfAborted } from "../experimental/runtime/src/errors.mjs";
import {
  sourcePath,
  sha256,
  recordDigest,
  readSnapshotBytes,
  decodeText,
  readHarnessRecord,
  writeHarnessRecord,
} from "./harness-snapshot.mjs";
import {
  createPixel,
  createModel,
  validatePixel,
  validateModel,
  editPixel,
  editModel,
  renderPixel,
  renderModel,
} from "./visual-assets.mjs";
import { encodePng } from "./visual-png.mjs";

export const VISUAL_LIMITS = Object.freeze({
  assets: 8,
  revisions: 128,
  operations: 512,
  draftBytes: 8 * 1024 * 1024,
  imageBytes: 32_000,
  resultBytes: 62_000,
});
const suffix = { pixel: ".rhtexture.json", model: ".rhmodel.json" };
const validators = { pixel: validatePixel, model: validateModel };
const editors = { pixel: editPixel, model: editModel };
const bytes = (value) => Buffer.byteLength(JSON.stringify(value));
const operationPath = (id) => `control/visual-operations/${sha256(id)}.json`;
const revisionPath = (record) =>
  `control/visual-assets/${sha256(record.path)}/${record.revision}-${sha256(record.operationId)}.json`;
function visualPath(kind, path) {
  sourcePath(path);
  check(
    Object.hasOwn(suffix, kind) && path.endsWith(suffix[kind]),
    "INVALID_VISUAL_PATH",
    `Use a visible ${suffix[kind]} project path`,
  );
}
function summary(kind, document, { region, elementOffset = 0 } = {}) {
  const texture = kind === "pixel" ? document : document.texture;
  const colorsUsed = [...new Set(texture.pixels)].sort((a, b) => a - b);
  const pixel = {
    width: texture.width,
    height: texture.height,
    palette: texture.palette,
    colorsUsed,
    opaquePixels: texture.pixels.filter(
      (index) => !texture.palette[index].endsWith("00"),
    ).length,
  };
  if (kind === "pixel") {
    if (region) {
      check(
        region.x + region.width <= texture.width &&
          region.y + region.height <= texture.height,
        "INVALID_ARGUMENTS",
        "Inspection region exceeds the texture",
      );
      pixel.region = {
        ...region,
        pixels: Array.from({ length: region.height }, (_, row) =>
          texture.pixels.slice(
            (region.y + row) * texture.width + region.x,
            (region.y + row) * texture.width + region.x + region.width,
          ),
        ),
      };
    }
    return pixel;
  }
  const elements = document.elements.slice(elementOffset, elementOffset + 8);
  return {
    texture: pixel,
    elementCount: document.elements.length,
    elementOffset,
    elements,
    nextElementOffset:
      elementOffset + elements.length < document.elements.length
        ? elementOffset + elements.length
        : null,
  };
}
function reduceImage(image) {
  const width = Math.max(1, Math.floor(image.width / 2)),
    height = Math.max(1, Math.floor(image.height / 2));
  const data = new Uint8ClampedArray(width * height * 4);
  for (let y = 0; y < height; y++)
    for (let x = 0; x < width; x++) {
      const from =
        (Math.floor((y * image.height) / height) * image.width +
          Math.floor((x * image.width) / width)) *
        4;
      data.set(image.data.subarray(from, from + 4), (y * width + x) * 4);
    }
  return { width, height, data };
}
function imagePreview(kind, document, view = {}) {
  let image =
    kind === "pixel"
      ? renderPixel(document, { checker: true })
      : renderModel(document, view);
  let png = encodePng(image);
  while (
    png.length > VISUAL_LIMITS.imageBytes &&
    Math.max(image.width, image.height) > 1
  ) {
    image = reduceImage(image);
    png = encodePng(image);
  }
  check(
    png.length <= VISUAL_LIMITS.imageBytes,
    "VISUAL_IMAGE_LIMIT",
    "Rendered image exceeds its byte limit",
  );
  return {
    mimeType: "image/png",
    data: png.toString("base64"),
    width: image.width,
    height: image.height,
    sha256: sha256(png),
    bytes: png.length,
  };
}

export class VisualService {
  constructor({ store, harnessService }) {
    check(
      store &&
        harnessService &&
        typeof harnessService.snapshot === "function" &&
        typeof harnessService.proposeChanges === "function",
      "INVALID_VISUAL_HOST",
      "Visual drafts need the host Harness service",
    );
    this.store = store;
    this.harness = harnessService;
  }
  async #context(ctx, kind, path, { mutate = false } = {}) {
    throwIfAborted(ctx.signal);
    visualPath(kind, path);
    const snapshot = await this.harness.snapshot(ctx.jobId);
    if (mutate) {
      check(
        snapshot.harness.mode === "execute",
        "TOOL_DENIED",
        "Visual changes require Execute mode",
      );
      check(
        snapshot.job.operations.some(
          (op) =>
            op.name === "harness.report_plan" && op.status === "completed",
        ),
        "PLAN_REQUIRED",
        "Record a plan before creating, editing or proposing a visual draft",
      );
      check(
        !snapshot.record.protectedPaths.includes(path),
        "HANDWRITTEN_PROTECTED",
        "This path retains a captured source lock",
      );
    }
    const entry = snapshot.entries.get(path);
    // Revalidate the original on every replay, including operations that edit
    // an already-created draft. The expected hash always stays the frozen one.
    if (entry)
      await readSnapshotBytes(snapshot.workspace, `source/${path}`, entry);
    return { ...snapshot, expectedSha256: entry?.sha256 ?? null };
  }
  async #operations(control) {
    let names;
    try {
      names = await fs.readdir(
        path.dirname(
          await control.resolve("control/visual-operations/inventory.json"),
        ),
      );
    } catch (error) {
      if (error.code === "ENOENT") return [];
      throw error;
    }
    check(
      names.length <= VISUAL_LIMITS.operations &&
        names.every((name) => /^[a-f0-9]{64}\.json$/.test(name)),
      "VISUAL_DRAFT_CHANGED",
      "Visual operation inventory changed",
    );
    const records = [];
    for (const name of names.sort()) {
      const record = await readHarnessRecord(
        control,
        `control/visual-operations/${name}`,
      );
      const { digest, ...committed } = record;
      check(
        record.schemaVersion === 1 &&
          digest === recordDigest(committed) &&
          name === `${sha256(record.operationId)}.json` &&
          /^[a-f0-9]{64}$/.test(record.inputDigest) &&
          Number.isInteger(record.revision) &&
          record.revision > 0 &&
          record.revision <= VISUAL_LIMITS.revisions &&
          Number.isInteger(record.sequence) &&
          record.sequence > 0 &&
          record.sequence <= VISUAL_LIMITS.operations &&
          Number.isInteger(record.documentBytes) &&
          record.documentBytes > 0,
        "VISUAL_DRAFT_CHANGED",
        "Visual operation commit changed",
      );
      visualPath(record.kind, record.path);
      records.push(record);
    }
    return records;
  }
  #latest(records) {
    const states = new Map(),
      revisions = new Set(),
      sequences = new Set();
    for (const record of records) {
      const revisionKey = `${record.path}:${record.revision}`;
      check(
        !revisions.has(revisionKey),
        "VISUAL_DRAFT_CHANGED",
        "Conflicting visual revisions",
      );
      check(
        !sequences.has(record.sequence) && record.sequence <= records.length,
        "VISUAL_DRAFT_CHANGED",
        "Visual commit sequence changed",
      );
      revisions.add(revisionKey);
      sequences.add(record.sequence);
      const current = states.get(record.path);
      if (!current || record.revision > current.revision)
        states.set(record.path, record);
    }
    check(
      states.size <= VISUAL_LIMITS.assets &&
        records.reduce((sum, item) => sum + item.documentBytes, 0) <=
          VISUAL_LIMITS.draftBytes,
      "VISUAL_DRAFT_LIMIT",
      "Job visual drafts exceeded their storage budget",
    );
    return states;
  }
  async #storageBudget(control, target, nextBytes) {
    const root = path.dirname(
      await control.resolve("control/visual-assets/inventory.json", {
        parents: true,
      }),
    );
    const directories = await fs.readdir(root, { withFileTypes: true });
    check(
      directories.length <= VISUAL_LIMITS.operations &&
        directories.every(
          (item) => item.isDirectory() && /^[a-f0-9]{64}$/.test(item.name),
        ),
      "VISUAL_DRAFT_CHANGED",
      "Visual revision directories changed",
    );
    let total = 0,
      count = 0,
      targetExists = false;
    for (const directory of directories) {
      const folder = `control/visual-assets/${directory.name}`;
      const names = await fs.readdir(
        path.dirname(await control.resolve(`${folder}/inventory.json`)),
      );
      for (const name of names) {
        check(
          /^[1-9][0-9]{0,2}-[a-f0-9]{64}\.json$/.test(name),
          "VISUAL_DRAFT_CHANGED",
          "Unexpected visual revision file",
        );
        const relative = `${folder}/${name}`,
          stat = await fs.lstat(await control.resolve(relative));
        check(
          stat.isFile() && stat.nlink <= 1,
          "VISUAL_DRAFT_CHANGED",
          "Expected a private visual revision file",
        );
        total += stat.size;
        count++;
        if (relative === target) targetExists = true;
        check(
          total <= VISUAL_LIMITS.draftBytes &&
            count <= VISUAL_LIMITS.operations,
          "VISUAL_DRAFT_LIMIT",
          "Visual storage budget includes interrupted revisions",
        );
      }
    }
    check(
      total + (targetExists ? 0 : nextBytes) <= VISUAL_LIMITS.draftBytes &&
        count + (targetExists ? 0 : 1) <= VISUAL_LIMITS.operations,
      "VISUAL_DRAFT_LIMIT",
      "Visual storage budget includes interrupted revisions",
    );
  }
  async #document(snapshot, record) {
    check(
      record.jobId === snapshot.job.id &&
        record.expectedSha256 ===
          (snapshot.entries.get(record.path)?.sha256 ?? null),
      "VISUAL_DRAFT_CHANGED",
      "Draft original source binding changed",
    );
    const stored = await readHarnessRecord(
      snapshot.control,
      revisionPath(record),
    );
    check(
      stored.schemaVersion === 1 &&
        stored.jobId === record.jobId &&
        stored.kind === record.kind &&
        stored.path === record.path &&
        stored.operationId === record.operationId &&
        stored.revision === record.revision &&
        stored.expectedSha256 === record.expectedSha256 &&
        stored.sha256 === record.sha256 &&
        sha256(JSON.stringify(stored.document)) === stored.sha256 &&
        bytes(stored.document) === record.documentBytes,
      "VISUAL_DRAFT_CHANGED",
      "Immutable visual revision changed",
    );
    return validators[record.kind](stored.document);
  }
  async #original(snapshot, kind, path) {
    const entry = snapshot.entries.get(path);
    check(
      entry,
      "SOURCE_FILE_UNAVAILABLE",
      "Visual path has no captured source or job draft",
    );
    let document;
    try {
      document = JSON.parse(
        decodeText(
          await readSnapshotBytes(snapshot.workspace, `source/${path}`, entry),
        ),
      );
    } catch (error) {
      if (error.code) throw error;
      check(
        false,
        "INVALID_VISUAL_ASSET",
        "Captured visual source is not JSON",
      );
    }
    return validators[kind](document);
  }
  #result(kind, path, revision, expectedSha256, document, options = {}) {
    const result = {
      label: "JOB_VISUAL_DRAFT",
      kind,
      path,
      revision,
      expectedSha256,
      sha256: sha256(JSON.stringify(document)),
      summary: {
        ...summary(kind, document, options),
        ...(kind === "model"
          ? {
              view: {
                yaw: options.view?.yaw ?? 45,
                pitch: options.view?.pitch ?? 30,
              },
            }
          : {}),
      },
      image: imagePreview(kind, document, options.view),
      originalSourceModified: false,
      buildValidated: false,
    };
    check(
      bytes(result) <= VISUAL_LIMITS.resultBytes,
      "VISUAL_RESULT_LIMIT",
      "Visual result exceeded its byte limit",
    );
    return result;
  }
  async #mutate(ctx, kind, args, action) {
    const snapshot = await this.#context(ctx, kind, args.path, {
      mutate: true,
    });
    check(
      typeof ctx.operationId === "string" &&
        /^[a-zA-Z0-9_-]{1,120}$/.test(ctx.operationId),
      "INVALID_OPERATION",
      "Visual changes need a durable operation identity",
    );
    const records = await this.#operations(snapshot.control),
      states = this.#latest(records);
    const inputDigest = recordDigest({ kind, action, args });
    const replay = records.find(
      (record) => record.operationId === ctx.operationId,
    );
    if (replay) {
      check(
        replay.inputDigest === inputDigest,
        "OPERATION_MISMATCH",
        "Visual operation reused with different arguments",
      );
      return this.#result(
        kind,
        args.path,
        replay.revision,
        replay.expectedSha256,
        await this.#document(snapshot, replay),
      );
    }
    const current = states.get(args.path);
    check(
      current || states.size < VISUAL_LIMITS.assets,
      "VISUAL_DRAFT_LIMIT",
      "Job already has eight visual drafts",
    );
    let document;
    if (action === "create") {
      check(
        !current && !snapshot.entries.has(args.path),
        "VISUAL_ASSET_EXISTS",
        "Create requires a new visible source path",
      );
      check(
        states.size < VISUAL_LIMITS.assets,
        "VISUAL_DRAFT_LIMIT",
        "Job already has eight visual drafts",
      );
      if (kind === "pixel") {
        document = createPixel(args.width ?? 16, args.height ?? 16);
        if (args.palette)
          document = validatePixel({ ...document, palette: args.palette });
      } else
        document = args.document ? validateModel(args.document) : createModel();
    } else {
      check(
        args.revision === (current?.revision ?? 0),
        "VISUAL_REVISION_CONFLICT",
        "Inspect the current revision before editing",
      );
      document = current
        ? await this.#document(snapshot, current)
        : await this.#original(snapshot, kind, args.path);
      document = editors[kind](document, args.operations);
    }
    validators[kind](document);
    const revision = (current?.revision ?? 0) + 1;
    const documentBytes = bytes(document);
    check(
      revision <= VISUAL_LIMITS.revisions &&
        records.length < VISUAL_LIMITS.operations &&
        records.reduce((sum, item) => sum + item.documentBytes, 0) +
          documentBytes <=
          VISUAL_LIMITS.draftBytes,
      "VISUAL_DRAFT_LIMIT",
      "Visual revision storage budget reached",
    );
    const result = this.#result(
      kind,
      args.path,
      revision,
      snapshot.expectedSha256,
      document,
    );
    throwIfAborted(ctx.signal);
    const record = {
      schemaVersion: 1,
      jobId: ctx.jobId,
      operationId: ctx.operationId,
      kind,
      path: args.path,
      revision,
      expectedSha256: snapshot.expectedSha256,
      sha256: result.sha256,
      document,
    };
    // Count orphaned revision files too. A crash before the commit must not
    // allow repeated retries to bypass the job's private byte/file budgets.
    await this.#storageBudget(
      snapshot.control,
      revisionPath(record),
      bytes(record),
    );
    throwIfAborted(ctx.signal);
    await writeHarnessRecord(snapshot.control, revisionPath(record), record);
    throwIfAborted(ctx.signal);
    const commit = {
      schemaVersion: 1,
      jobId: ctx.jobId,
      operationId: ctx.operationId,
      inputDigest,
      kind,
      path: args.path,
      revision,
      sequence: records.length + 1,
      expectedSha256: snapshot.expectedSha256,
      sha256: result.sha256,
      documentBytes,
    };
    commit.digest = recordDigest(commit);
    // This immutable commit is the visibility boundary. Cancellation after it
    // leaves a recoverable private revision and can never publish user source.
    await writeHarnessRecord(
      snapshot.control,
      operationPath(ctx.operationId),
      commit,
    );
    return result;
  }
  create(ctx, kind, args) {
    return this.#mutate(ctx, kind, args, "create");
  }
  edit(ctx, kind, args) {
    return this.#mutate(ctx, kind, args, "edit");
  }
  async inspect(ctx, kind, args) {
    const snapshot = await this.#context(ctx, kind, args.path);
    const records = await this.#operations(snapshot.control),
      latest = this.#latest(records).get(args.path);
    const revision =
      ctx.replayReceipt?.revision ?? args.revision ?? latest?.revision ?? 0;
    const record = records.find(
      (item) => item.path === args.path && item.revision === revision,
    );
    check(
      revision === 0 || record,
      "VISUAL_REVISION_CONFLICT",
      "Requested revision is unavailable",
    );
    const document =
      revision === 0
        ? await this.#original(snapshot, kind, args.path)
        : await this.#document(snapshot, record);
    throwIfAborted(ctx.signal);
    return this.#result(
      kind,
      args.path,
      revision,
      snapshot.expectedSha256,
      document,
      args,
    );
  }
  async propose(ctx, kind, args) {
    const snapshot = await this.#context(ctx, kind, args.path, {
      mutate: true,
    });
    const records = await this.#operations(snapshot.control),
      latest = this.#latest(records).get(args.path);
    check(
      latest && latest.revision === args.revision,
      "VISUAL_REVISION_CONFLICT",
      "Propose the latest inspected draft revision",
    );
    const document = await this.#document(snapshot, latest);
    throwIfAborted(ctx.signal);
    return this.harness.proposeChanges(ctx, {
      summary: args.summary,
      changes: [
        {
          path: args.path,
          expected_sha256: latest.expectedSha256,
          content: `${JSON.stringify(document, null, 2)}\n`,
        },
      ],
    });
  }
  async preview(jobId) {
    const snapshot = await this.harness.snapshot(jobId);
    const states = this.#latest(await this.#operations(snapshot.control));
    const result = [];
    for (const record of [...states.values()]
      .sort((a, b) => b.sequence - a.sequence)
      .slice(0, 2)) {
      const original = snapshot.entries.get(record.path);
      if (original)
        await readSnapshotBytes(
          snapshot.workspace,
          `source/${record.path}`,
          original,
        );
      const document = await this.#document(snapshot, record);
      result.push({
        kind: record.kind,
        path: record.path,
        revision: record.revision,
        sha256: record.sha256,
        summary: summary(record.kind, document),
        image: imagePreview(record.kind, document),
        document,
      });
    }
    // Documents help an editor import a draft, but the public poll response
    // remains bounded even for maximum textures. The PNG always remains visible.
    if (bytes(result) > 160_000)
      for (const artifact of result) delete artifact.document;
    check(
      bytes(result) <= 160_000,
      "VISUAL_RESULT_LIMIT",
      "Public visual previews exceeded their byte budget",
    );
    return result;
  }
}
