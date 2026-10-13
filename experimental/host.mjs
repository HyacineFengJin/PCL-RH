/** Private stdio host, owned by Tauri. Third-party extensions only reach the
 * declarative ExtensionHost; neither this dispatcher nor Pi tools are exposed
 * to their cards. Source transformations reuse the supplied domain modules. */
import * as fs from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { constants } from "node:fs";
import path from "node:path";
import readline from "node:readline";
import { prepareCodeJob } from "../code/agent-adapter.mjs";
import { HarnessService } from "../code/harness-service.mjs";
import { HarnessToolCatalog } from "../code/harness-tool-catalog.mjs";
import { VisualService } from "../code/visual-service.mjs";
import { registerVisualTools } from "./runtime/src/visual-tools.mjs";
import { readJobSnapshot } from "./job-snapshot.mjs";
import { registerHarnessTools } from "./runtime/src/harness-tools.mjs";
import { fileURLToPath } from "node:url";
import {
  AgentRuntime,
  createDefaultTools,
  registerDomainAdapters,
  HostReviewService,
  registerReviewPreviewTools,
  LivePiProvider,
  projectPublicJobSummary,
} from "./runtime/src/index.mjs";
import {
  getPiCatalog,
  publicPiSelection,
} from "./runtime/src/providers/pi-selection.mjs";
import { SubprocessSupervisor } from "./runtime/src/subprocesses.mjs";
import {
  captureSourceArtifact,
  sha256,
} from "./runtime/src/artifact-snapshot.mjs";
import { ExtensionHost } from "./extensions/src/host.mjs";
import { readManifest } from "./extensions/src/manifest.mjs";
import { readPackage } from "./extensions/src/package.mjs";
import { inspectForeignManifest } from "./extensions/src/compatibility.mjs";
import {
  foreignPackageFormat,
  inspectForeignPackageFile,
} from "./extensions/src/foreign-package.mjs";
import { AiPresets } from "./ai-presets.mjs";
import { prepareIdle } from "./idle-state.mjs";
import {
  captureIndexedSource,
  indexPage,
  readIndexFile,
  searchIndex,
} from "./runtime/vendor/maker/source-index.mjs";
import { MakerProjects } from "./runtime/vendor/maker/projects.mjs";
import {
  snapshotProtection,
  indexedProtection,
} from "./runtime/vendor/maker/protection.mjs";
import {
  MakerThreads,
  makerPlanTool,
} from "./runtime/vendor/maker/threads.mjs";
import { MakerWorkspace } from "./runtime/vendor/maker/workspace.mjs";
import { PorterProjects } from "./porter-projects.mjs";
import { PorterVersions } from "./porter-versions.mjs";
import { resolvePorterOrigin } from "./porter-origins.mjs";
import {
  compareProjectSources,
  compareProjectFile,
} from "./runtime/vendor/maker/source-compare.mjs";

const code = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(process.argv[2]);
await fs.mkdir(root, { recursive: true, mode: 0o700 });
let selection = null,
  closing = false;
const presets = await new AiPresets(root).init();
// Per-job snapshots outlive edits, preset deletion and the engine's stop flag.
// Keys never enter durable job input, receipts or renderer projections.
const liveRuns = new Map();
// Creation and grant registration happen before runtime.start(). Keep those
// in-process jobs visible to admission/recovery until the runtime owns them.
const pendingJobs = new Set();
const hasCurrentJob = (id) => pendingJobs.has(id) || runtime.hasActiveJob(id);
const adapters = registerDomainAdapters(createDefaultTools());
adapters.registry.register(makerPlanTool);
const bridge = new SubprocessSupervisor({
  trustedCommands: Object.fromEntries(
    [
      ["catalog", path.join(code, "python-host.py"), "catalog"],
      [
        "read_source",
        path.join(code, "python-host.py"),
        "porter.read_directory",
      ],
    ].map(([name, script, operation]) => [
      name,
      { executable: "/usr/bin/python3", args: ["-I", "-B", script, operation] },
    ]),
  ),
});
async function domain(name, args) {
  const result = await bridge.run(name, {
    cwd: root,
    input: JSON.stringify(args),
    maxOutputBytes: 256000,
    timeoutMs: 10000,
  });
  const envelope = JSON.parse(result.stdout);
  if (result.status !== "completed" || !envelope.ok)
    throw new Error(
      envelope.error?.message ?? envelope.error ?? "Domain adapter failed",
    );
  return envelope.result;
}
// This driver calls real deterministic domain tools; it is not an AI simulator.
class TemplateProvider {
  id = "pcl-template";
  async run(ctx) {
    if (ctx.job.profile === "maker") {
      await ctx.invoke("plan", "maker.plan", { spec: ctx.job.input.spec });
      return ctx.invoke("generate", "maker.generate", {
        spec: ctx.job.input.spec,
      });
    }
    const { files, targetId, rights, acknowledgeBeta } = ctx.job.input;
    return ctx.invoke("inspect", "porter.inspect", {
      files,
      targetId,
      rights,
      acknowledgeBeta,
    });
  }
}
const live = new LivePiProvider({
  enabled: true,
  getSelection: (job) => liveRuns.get(job.id)?.selection,
  getLimits: (job) => liveRuns.get(job.id)?.limits,
});
const runtime = await new AgentRuntime({
  root: path.join(root, "jobs"),
  tools: adapters.registry,
  providers: [new TemplateProvider(), live],
}).init();
const reviews = new HostReviewService({ store: runtime.store });
const harness = new HarnessService({ store: runtime.store });
const harnessTools = new HarnessToolCatalog(adapters.registry);
registerHarnessTools(harnessTools, harness);
const visualAssets = new VisualService({
  store: runtime.store,
  harnessService: harness,
});
registerVisualTools(harnessTools.forPlugin("rh.pixel"), visualAssets, "pixel");
registerVisualTools(harnessTools.forPlugin("rh.model"), visualAssets, "model");
// Image previews change only after completed visual operations. Chat streaming
// must not rerasterize a model on every status poll. This small cache dies with
// the idle host; each entry is bounded and contains no credentials.
const visualPreviews = new Map();
async function visualPreview(job) {
  const operations = job.operations.filter(
    (op) => /^(pixel|model)\./.test(op.name) && op.status === "completed",
  );
  if (!operations.length) return {};
  const key = JSON.stringify(operations.map((op) => [op.id, op.result]));
  const cached = visualPreviews.get(job.id);
  if (cached?.key === key) return cached.value;
  let value;
  try {
    value = { visualArtifacts: await visualAssets.preview(job.id) };
  } catch {
    value = {
      visualArtifacts: [],
      visualError:
        "Visual preview is unavailable; inspect the task before trying again.",
    };
  }
  if (visualPreviews.size >= 8)
    visualPreviews.delete(visualPreviews.keys().next().value);
  visualPreviews.set(job.id, { key, value });
  return value;
}
const porterProjects = await new PorterProjects(root, {
  getJob: (id) => runtime.getJob(id),
  hasActiveJob: hasCurrentJob,
}).init();
registerReviewPreviewTools(adapters.registry, reviews);
const porterVersions = new PorterVersions({
  projects: porterProjects,
  store: runtime.store,
  reviews,
});
for (const name of ["ask_user", "report_progress"]) {
  const fields =
    name === "ask_user"
      ? ["question", "options"]
      : ["summary", "completed", "remaining", "limitations"];
  adapters.registry.register({
    name: `porter.${name}`,
    replaySafety: "manual",
    description:
      name === "ask_user"
        ? "Persist a question for the user and end this project round. The user can answer and start a new round."
        : "Record an unverified progress summary with completed work, remaining work and limitations in the Porter project.",
    inputSchema: {
      type: "object",
      properties: Object.fromEntries(
        fields.map((field) => [
          field,
          ["question", "summary"].includes(field)
            ? { type: "string", maxLength: 2000 }
            : {
                type: "array",
                maxItems: field === "options" ? 6 : 16,
                items: {
                  type: "string",
                  maxLength: field === "options" ? 200 : 500,
                },
              },
        ]),
      ),
      required: name === "ask_user" ? ["question"] : fields,
      additionalProperties: false,
    },
    validate(args) {
      if (
        !args ||
        typeof args !== "object" ||
        Array.isArray(args) ||
        Object.keys(args).some((k) => !fields.includes(k))
      )
        throw new Error("Invalid Porter discussion tool arguments");
    },
    execute: (args, ctx) =>
      name === "ask_user"
        ? porterProjects.askUser(ctx.jobId, args)
        : porterProjects.reportProgress(ctx.jobId, args),
  });
}
// The new Porter recipes operate on the snapshot and path grants imported by
// the user. A model can propose changes, but cannot approve them or import files.
for (const recipe of ["metadata", "identifier"]) {
  adapters.registry.register({
    name: `porter.propose_${recipe}`,
    replaySafety: "read-only",
    description:
      recipe === "metadata"
        ? "Propose limited Fabric identity fields for an existing NeoForge manifest."
        : "Propose only Fabric/Yarn 1.20.6 → 1.21 Identifier factory edits. No full conversion.",
    inputSchema: {
      type: "object",
      properties: {},
      additionalProperties: false,
    },
    validate: (args) => {
      if (!args || Object.keys(args).length)
        throw new Error("This tool takes no arguments");
    },
    async execute(_args, ctx) {
      return reviews.previewPorterRecipeFromTool(ctx, recipe);
    },
  });
}
let extensions = new ExtensionHost();
let installed = [],
  storeWarning = null,
  safeMode = false;
const extensionFile = path.join(root, "extensions.json");
async function boundedRead(file, max = 128000) {
  const handle = await fs.open(file, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size > max)
      throw new Error("Expected a bounded regular file");
    const bytes = await handle.readFile();
    if (bytes.length > max) throw new Error("File exceeds limit");
    return bytes.toString("utf8");
  } finally {
    await handle.close();
  }
}
function parseExtension(source) {
  return JSON.parse(source).packageFormat === "pcl-linux.declarative-package"
    ? readPackage(source)
    : readManifest(source);
}
function prepareExtension(source) {
  return JSON.parse(source).packageFormat === "pcl-linux.declarative-package"
    ? extensions.preparePackageReview(source)
    : extensions.prepareReview(source);
}
try {
  const saved = JSON.parse(await boundedRead(extensionFile, 1000000));
  if (
    saved.schemaVersion !== 1 ||
    !Array.isArray(saved.entries) ||
    saved.entries.length > 32
  )
    throw new Error("Invalid extension store");
  for (const record of saved.entries) {
    const loaded = parseExtension(record.source);
    if (
      loaded.manifest.id !== record.id ||
      loaded.digest !== record.digest ||
      !Array.isArray(record.grants) ||
      typeof record.enabled !== "boolean"
    )
      throw new Error("Extension store content changed");
    const declared = loaded.manifest.capabilities.map((c) => c.id);
    if (record.grants.some((c) => !declared.includes(c)))
      throw new Error("Invalid stored grant");
    const review = prepareExtension(record.source);
    extensions.confirmReview(review.token, declared);
    for (const id of declared)
      if (!record.grants.includes(id)) extensions.revoke(record.id, id);
    if (!record.enabled) extensions.disable(record.id);
  }
  installed = saved.entries;
} catch (error) {
  if (error.code !== "ENOENT") {
    storeWarning =
      "Extension store unreadable; retained for recovery. No grants loaded.";
    extensions = new ExtensionHost();
    safeMode = true;
    extensions.setSafeMode(true);
  }
}
async function persistExtensions() {
  if (storeWarning) throw new Error(storeWarning);
  const entries = installed.map((record) => {
    const state = extensions.inspect(record.id);
    return { ...record, grants: state.grants, enabled: record.enabled };
  });
  const text = JSON.stringify({ schemaVersion: 1, entries });
  if (Buffer.byteLength(text) > 1000000)
    throw new Error("Extension store exceeds 1 MB");
  const temporary = extensionFile + ".new";
  await fs.writeFile(temporary, text, { flag: "wx", mode: 0o600 });
  await fs.rename(temporary, extensionFile);
  installed = entries;
}
const pendingExtensions = new Map();
const sourceImports = new Map();
async function listJobs() {
  const entries = await fs.readdir(runtime.store.root);
  const jobs = [];
  for (const id of entries
    .filter((v) => /^[a-f0-9-]{36}$/.test(v))
    .slice(-100)) {
    try {
      const job = await runtime.getJob(id);
      jobs.push({
        ...projectPublicJobSummary(job),
        ...(["queued", "running"].includes(job.status) && !hasCurrentJob(id)
          ? { status: "interrupted" }
          : {}),
        mode: job.provider === "pcl-template" ? "template" : "live",
      });
    } catch {
      /* Retain corrupt jobs without inventing a completed status. */
    }
  }
  return jobs.sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));
}
async function artifact(jobId, operationId) {
  const job = await runtime.getJob(jobId);
  const snapshot = await captureSourceArtifact(runtime.store, job, operationId);
  const workspace = await runtime.store.workspace(jobId);
  return {
    job,
    snapshot,
    directory: path.join(workspace.root, snapshot.directory),
  };
}
async function indexedArtifact(jobId, operationId) {
  return captureIndexedSource(
    runtime.store,
    await runtime.getJob(jobId),
    operationId,
  );
}
const projects = await new MakerProjects(root, indexedArtifact).init();
const makerWorkspace = await new MakerWorkspace(root, projects).init();
const makerThreads = await new MakerThreads(root, {
  projects,
  job: async (id) => {
    const value = await runtime.getJob(id);
    const directory = path.join(
      runtime.store.directory(id),
      "review-control/reviews",
    );
    const reviewIds = (
      await fs.readdir(directory).catch((error) => {
        if (error.code === "ENOENT") return [];
        throw error;
      })
    )
      .filter((name) => name.endsWith(".json"))
      .map((name) => name.slice(0, -5));
    return {
      ...value,
      reviewIds,
      status:
        ["queued", "running"].includes(value.status) && !hasCurrentJob(id)
          ? "interrupted"
          : value.status,
    };
  },
  create: (args) =>
    dispatch(args.id ? "project_continue" : "job_create", {
      ...args,
      profile: "maker",
      mode: "live",
      deferThreadStart: true,
    }),
  start: async (jobId) => {
    const job = await runtime.getJob(jobId);
    const captured = liveRuns.get(jobId);
    return startCapturedJob(job, captured);
  },
  abort: async (jobId) => {
    pendingJobs.delete(jobId);
    liveRuns.delete(jobId);
    await runtime.cancel(jobId);
  },
  review: (jobId, reviewId) => reviews.getReview(jobId, reviewId),
  apply: async (jobId, reviewId, digest) => {
    const approval = await reviews.approveReview(jobId, {
      reviewId,
      expectedDigest: digest,
    });
    return reviews.applyApprovedReview(jobId, {
      reviewId,
      approvalToken: approval.approvalToken,
    });
  },
}).init();
async function admitMakerThreadJob(job, preset, deferred) {
  if (!deferred) return startCapturedJob(job, preset);
  pendingJobs.add(job.id);
  liveRuns.set(job.id, { selection: preset.selection, limits: preset.limits });
  return { jobId: job.id };
}
async function startCapturedJob(job, preset) {
  if (preset) {
    selection = preset.selection;
    liveRuns.set(job.id, {
      selection: preset.selection,
      limits: preset.limits,
    });
  }
  try {
    void runtime
      .start(job.id)
      .catch(() => {})
      .finally(() => liveRuns.delete(job.id));
    pendingJobs.delete(job.id);
  } catch (error) {
    liveRuns.delete(job.id);
    pendingJobs.delete(job.id);
    await runtime.cancel(job.id);
    throw error;
  }
  return { jobId: job.id };
}
async function createPorterRound(input, permittedPaths, mode) {
  if (
    (await listJobs()).filter((j) => ["queued", "running"].includes(j.status))
      .length >= 2
  )
    throw new Error("The shared runtime already has two active jobs");
  const preset = mode === "live" ? presets.selected() : null;
  if (mode === "live" && !preset)
    throw new Error("Save and select an AI preset first");
  const job = await runtime.createJob({
    profile: "porter",
    input,
    provider: mode === "live" ? live.id : "pcl-template",
  });
  pendingJobs.add(job.id);
  try {
    if (permittedPaths.length)
      await reviews.registerPorterSource(job.id, {
        files: input.files,
        permittedPaths,
      });
    if (preset) {
      selection = preset.selection;
      liveRuns.set(job.id, {
        selection: preset.selection,
        limits: preset.limits,
      });
    }
    return job;
  } catch (error) {
    pendingJobs.delete(job.id);
    await runtime.cancel(job.id);
    throw error;
  }
}
// Native project membership alone cannot authenticate a renderer-supplied job
// reference. Match it against provenance captured when Rust prepared the task.
function checkRhOwner(job, args, conversation = false) {
  if (
    !args.projectId ||
    job.input?.rhCode?.projectId !== args.projectId ||
    ((conversation || args.conversationId !== undefined) &&
      (!args.conversationId ||
        job.input.rhCode.conversationId !== args.conversationId))
  )
    throw new Error("RH Code task belongs to another project or conversation");
}
async function dispatch(operation, a) {
  if (closing && operation !== "shutdown")
    throw new Error("Experimental host is closing");
  switch (operation) {
    case "idle_prepare":
      return prepareIdle({
        runtime,
        reviews,
        children: [reviews, adapters, bridge],
        pendingJobs,
        liveRuns,
        pendingExtensions,
        sourceImports,
        safeMode,
        selection,
        preset: presets.selected(),
      });
    case "idle_resume": {
      const preset = presets.selected();
      // An external edit/deletion must not resurrect another provider choice.
      if (preset?.id === a.presetId) selection = preset.selection;
      return true;
    }
    case "rh_code_run":
      return prepareCodeJob(a, {
        runtime,
        preset: presets.selected(),
        provider: live.id,
        jobs: await listJobs(),
        admit: admitMakerThreadJob,
      });
    case "rh_code_start": {
      if (!pendingJobs.has(a.jobId))
        throw new Error("RH Code task is not waiting for admission");
      const job = await runtime.getJob(a.jobId);
      checkRhOwner(job, a, true);
      return startCapturedJob(job, liveRuns.get(a.jobId));
    }
    case "rh_code_harness_reviews": {
      checkRhOwner(await runtime.getJob(a.jobId), a);
      return harness.listReviews(a.jobId);
    }
    case "rh_code_harness_export": {
      checkRhOwner(await runtime.getJob(a.jobId), a);
      return harness.exportProposal(a.jobId, {
        proposalId: a.proposalId,
        expectedDigest: a.expectedDigest,
      });
    }
    case "rh_code_harness_applied": {
      checkRhOwner(await runtime.getJob(a.jobId), a);
      return harness.markApplied(a.jobId, {
        proposalId: a.proposalId,
        expectedDigest: a.expectedDigest,
      });
    }
    case "status":
      return {
        versions: {
          extensions: "0.6.2",
          maker: "1.1-preview",
          porter: "0.6.0",
          runtime: "v7",
          pi: "1.0.4",
        },
        jobs: await listJobs(),
        liveConfigured: !!selection,
        liveAvailable: !!presets.selected(),
        liveSelection: publicPiSelection(selection),
        aiPresets: presets.view(),
        harnessTools: harnessTools.view(),
        liveTested: false,
        workspace: root,
        storeWarning,
        extensions: extensions.list(),
      };
    case "catalog":
      return domain("catalog", {});
    case "maker_thread_read":
      return makerThreads.read(a.threadId);
    case "maker_threads": {
      const listing = await makerThreads.list();
      const pendingJobs = [];
      const threads = [];
      for (const record of listing.threads) {
        const last = record.rounds.at(-1);
        let summary = null;
        if (last) {
          const job = await makerThreads.job(last.jobId);
          summary = {
            ...(await runtime.getPublicJobSummary(last.jobId)),
            status: job.status,
          };
          const pending = [];
          for (const reviewId of job.reviewIds) {
            const review = await reviews.getReview(job.id, reviewId);
            if (["pending", "approved"].includes(review.status))
              pending.push(reviewId);
          }
          if (
            pending.length ||
            (!record.projectId &&
              job.operations.some((op) => op.result?.outputDirectory))
          )
            pendingJobs.push(job.id);
        }
        threads.push({
          ...record,
          notes: "",
          rounds: last
            ? [
                {
                  jobId: last.jobId,
                  mode: last.mode,
                  checkpointId: last.checkpointId,
                  request: last.request.slice(0, 240),
                },
              ]
            : [],
          roundCount: record.rounds.length,
          summary,
        });
      }
      return { threads, errors: listing.errors, pendingJobs };
    }
    case "maker_thread_run":
      return makerThreads.run(a);
    case "maker_thread_apply":
      return makerThreads.approve(a);
    case "maker_thread_archive":
      return makerThreads.archive(a);
    case "projects_list":
      return { projects: await projects.list() };
    case "project_create":
      return projects.create(a);
    case "project_update":
      return projects.update(a);
    case "project_units":
      return makerWorkspace.list(a);
    case "project_unit_read":
      return makerWorkspace.read(a);
    case "project_unit_save":
      return makerWorkspace.save(a);
    case "project_protection":
      return indexedProtection(await projects.source(a));
    case "project_files":
      return indexPage(await projects.source(a), a);
    case "project_file_read":
      return readIndexFile(await projects.source(a), a);
    case "project_search":
      return searchIndex(await projects.source(a), a);
    case "project_compare":
      return compareProjectSources(projects, a);
    case "project_compare_file":
      return compareProjectFile(projects, a);
    case "project_open": {
      const source = await projects.source(a, artifact);
      return {
        workflow: source.point.workflow,
        jobId: source.point.jobId,
        operationId: source.point.operationId,
        ...(source.point.workflow === "maker"
          ? {
              spec: JSON.parse(
                source.snapshot.files["creator-spec.json"]?.toString("utf8") ??
                  "null",
              ),
            }
          : {}),
      };
    }
    case "project_continue": {
      if (
        typeof a.prompt !== "string" ||
        !a.prompt.trim() ||
        a.prompt.length > 12000
      )
        throw new Error("Describe the project change in 1–12000 characters");
      const preset = presets.selected();
      if (!preset) throw new Error("Save and select an AI preset first");
      if (
        (await listJobs()).filter((j) =>
          ["queued", "running"].includes(j.status),
        ).length >= 2
      )
        throw new Error("The shared runtime already has two active jobs");
      const source = await projects.source(a, artifact);
      if (source.project.archived)
        throw new Error("Unarchive the project before continuing");
      if (source.point.workflow !== "maker")
        throw new Error(
          "Continue migrated source in its original workflow; Maker continuation requires a Maker source project",
        );
      const spec = JSON.parse(
        source.snapshot.files["creator-spec.json"]?.toString("utf8") ?? "null",
      );
      if (!spec) throw new Error("Source has no Maker specification");
      const reviewId = randomUUID(),
        operationId = `host-${reviewId}`,
        outputDirectory = `reviewed/${reviewId}`;
      // Copy only receipt-verified bytes into a fresh job. The model receives
      // project context and its own source operation, never another job's paths.
      const job = await runtime.createJob({
        profile: "maker",
        provider: live.id,
        input: {
          spec,
          prompt: a.prompt,
          ...(a.makerThread
            ? {
                makerThread: {
                  ...a.makerThread,
                  protectedPaths: snapshotProtection(source.snapshot),
                },
              }
            : {}),
          sourceOperationId: operationId,
          sourceDirectory: outputDirectory,
          sourceInventory: source.snapshot.inventory.map((entry) => ({
            ...entry,
            expected_sha256: entry.sha256,
          })),
          project: {
            id: source.project.id,
            name: source.project.name,
            notes: source.project.notes,
            checkpointId: source.point.id,
          },
        },
      });
      try {
        const workspace = await runtime.store.workspace(job.id),
          artifacts = [];
        for (const [name, bytes] of Object.entries(source.snapshot.files)) {
          const written = await workspace.writeBytes(
            `${outputDirectory}/${name}`,
            bytes,
          );
          artifacts.push({ ...written, sha256: sha256(bytes) });
        }
        job.operations.push({
          id: operationId,
          name: "maker.edit_source",
          status: "completed",
          result: {
            status: "reviewed_copy_created",
            reviewId,
            outputDirectory,
            artifacts,
            originalSourceModified: false,
            buildValidated: false,
            verificationState: "not_run",
            parentFingerprint: source.point.fingerprint,
          },
        });
        await runtime.store.save(job);
        return {
          ...(await admitMakerThreadJob(job, preset, a.deferThreadStart)),
          operationId,
          spec,
        };
      } catch (error) {
        await runtime.cancel(job.id);
        throw error;
      }
    }
    case "extensions_list":
      return { entries: extensions.list(), safeMode, warning: storeWarning };
    case "extensions_review": {
      // A foreign archive can only produce a read-only report. It never reaches
      // the native consent map, even if its plugin.json resembles an RH package.
      if (foreignPackageFormat(a.path))
        return inspectForeignPackageFile(a.path);
      const source = a.path ? await boundedRead(a.path) : a.source;
      // Foreign metadata never enters the install store or pending consent map.
      // Inspection remains available even when the declarative store is damaged.
      const compatibility = inspectForeignManifest(source);
      if (compatibility) return compatibility;
      if (storeWarning) throw new Error(storeWarning);
      const review = prepareExtension(source);
      if (pendingExtensions.size >= 64) pendingExtensions.clear();
      pendingExtensions.set(review.token, source);
      return review;
    }
    case "extensions_confirm": {
      if (storeWarning) throw new Error(storeWarning);
      const source = pendingExtensions.get(a.token);
      pendingExtensions.delete(a.token);
      if (!source) throw new Error("Extension review expired");
      const result = extensions.confirmReview(a.token, a.grants);
      installed = installed.filter((v) => v.id !== result.id);
      installed.push({
        id: result.id,
        digest: result.digest,
        source,
        grants: result.grants,
        enabled: true,
      });
      try {
        await persistExtensions();
      } catch (error) {
        extensions.disable(result.id);
        throw error;
      }
      return result;
    }
    case "extensions_cancel":
      pendingExtensions.delete(a.token);
      return extensions.cancelReview(a.token);
    case "extensions_revoke": {
      const result = extensions.revoke(a.id, a.capability);
      await persistExtensions();
      return result;
    }
    case "extensions_disable": {
      const result = extensions.disable(a.id);
      installed.find((v) => v.id === a.id).enabled = false;
      await persistExtensions();
      return result;
    }
    case "extensions_rereview": {
      const record = installed.find((v) => v.id === a.id);
      if (!record) throw new Error("Unknown extension");
      return dispatch("extensions_review", { source: record.source });
    }
    case "extensions_safe_mode":
      if (storeWarning) throw new Error(storeWarning);
      safeMode = a.enabled;
      return extensions.setSafeMode(a.enabled);
    case "extensions_cards":
      extensions.setContext(a.context ?? null);
      return extensions.cards(a.slot);
    case "extensions_action":
      extensions.setContext(a.context ?? null);
      return extensions.commitAction(
        extensions.prepareAction(a.extensionId, a.cardId, a.actionId).token,
      );
    case "source_import": {
      const result = await domain("read_source", { directory: a.directory });
      if (sourceImports.size >= 16) sourceImports.clear();
      const id = randomUUID();
      sourceImports.set(id, result.files);
      return { sourceId: id, files: result.files, skipped: result.skipped };
    }
    case "provider_catalog":
      return getPiCatalog();
    case "provider_presets":
      return presets.view();
    case "provider_preset_save":
      return presets.save(a);
    case "provider_preset_select":
      return presets.select(a.id);
    case "provider_preset_delete":
      return presets.remove(a.id);
    case "provider_start": {
      const record = presets.selected();
      if (!record) throw new Error("Save and select an AI preset first");
      selection = record.selection;
      return {
        liveConfigured: true,
        liveSelection: publicPiSelection(selection),
      };
    }
    case "provider_stop":
      selection = null;
      await Promise.all([...liveRuns.keys()].map((id) => runtime.cancel(id)));
      return { liveConfigured: false, liveSelection: null };
    case "porter_origin_resolve":
      return resolvePorterOrigin(a.url);
    case "porter_projects":
      return porterProjects.list();
    case "porter_project_read":
      return porterProjects.read(a.projectId);
    case "porter_project_versions":
      return porterVersions.list(a.projectId);
    case "porter_project_compare":
      return porterVersions.compare(a.projectId, a.left, a.right);
    case "porter_project_create": {
      const catalog = await domain("catalog", {});
      if (!catalog.targets.some((t) => t.id === a.targetId))
        throw new Error("Choose a target from the offline catalog");
      const files = a.sourceId ? sourceImports.get(a.sourceId) : null;
      if (a.sourceId && !files)
        throw new Error("Re-import the source directory");
      if (!files && !a.origin?.url)
        throw new Error("Import source or provide a mod project link");
      return porterProjects.create({
        ...a,
        source: files
          ? { files, permittedPaths: a.permittedPaths || [] }
          : null,
      });
    }
    case "porter_project_message":
      return porterProjects.addMessage(a.projectId, a);
    case "porter_project_message_resolve":
      return porterProjects.resolveMessage(a.projectId, a);
    case "porter_project_archive":
      return porterProjects.archive(a.projectId, a);
    case "porter_project_configure": {
      const catalog = await domain("catalog", {});
      if (!catalog.targets.some((t) => t.id === a.targetId))
        throw new Error("Choose a target from the offline catalog");
      const current = await porterProjects.read(a.projectId);
      const files = a.sourceId
        ? sourceImports.get(a.sourceId)
        : current.snapshot?.files;
      if (a.sourceId && !files)
        throw new Error("Re-import the source directory");
      return porterProjects.configure(a.projectId, {
        ...a,
        source: files
          ? { files, permittedPaths: a.permittedPaths || [] }
          : null,
      });
    }
    case "porter_project_source": {
      let files, label;
      if (a.sourceId) {
        files = sourceImports.get(a.sourceId);
        if (!files) throw new Error("Re-import the source directory");
        label = "imported-source";
      } else {
        const bound = await artifact(a.jobId, a.operationId);
        if (
          bound.job.profile !== "porter" ||
          bound.job.input.porterProject?.id !== a.projectId
        )
          throw new Error("The source copy belongs to a different project");
        files = Object.fromEntries(
          Object.entries(bound.snapshot.files).map(([name, bytes]) => [
            name,
            bytes.toString("utf8"),
          ]),
        );
        label = `${a.jobId}:${a.operationId}`;
      }
      return porterProjects.setSource(a.projectId, {
        revision: a.revision,
        source: { files, label, permittedPaths: a.permittedPaths || [] },
      });
    }
    case "porter_project_round": {
      let created;
      try {
        await porterProjects.read(a.projectId);
        const result = await porterProjects.beginRound(
          a.projectId,
          a,
          async (...args) => {
            created = await createPorterRound(...args);
            return created;
          },
        );
        void runtime
          .start(result.jobId)
          .catch(() => {})
          .finally(async () => {
            liveRuns.delete(result.jobId);
            await porterProjects.reconcile(result.projectId).catch(() => {});
          });
        pendingJobs.delete(result.jobId);
        return result;
      } catch (error) {
        if (created) {
          pendingJobs.delete(created.id);
          liveRuns.delete(created.id);
          await runtime.cancel(created.id);
        }
        throw error;
      }
    }
    case "job_create": {
      if (
        !["maker", "porter"].includes(a.profile) ||
        !["template", "live"].includes(a.mode)
      )
        throw new Error("Choose a supported workflow");
      const preset = a.mode === "live" ? presets.selected() : null;
      if (a.mode === "live" && !preset)
        throw new Error("Save and select an AI preset first");
      if (
        (await listJobs()).filter((j) =>
          ["queued", "running"].includes(j.status),
        ).length >= 2
      )
        throw new Error("The shared runtime already has two active jobs");
      let files;
      if (a.profile === "porter") {
        files = sourceImports.get(a.sourceId);
        if (!files) throw new Error("Re-import the source directory");
        if (
          a.identifierProfile != null &&
          a.identifierProfile !== "fabric-yarn-1.20.6-to-1.21-identifier-v1"
        )
          throw new Error("Choose the supported Identifier source profile");
      }
      const input =
        a.profile === "maker"
          ? {
              spec: a.spec,
              ...(a.mode === "live"
                ? {
                    prompt: a.prompt,
                    ...(a.makerThread ? { makerThread: a.makerThread } : {}),
                  }
                : {}),
            }
          : {
              files,
              targetId: a.targetId,
              rights: a.rights,
              acknowledgeBeta: a.acknowledgeBeta,
              ...(a.identifierProfile
                ? { identifierProfile: a.identifierProfile }
                : {}),
              ...(a.mode === "live" ? { prompt: a.prompt } : {}),
            };
      // Template scanner rejects AI prompt as an unknown tool argument.
      const job = await runtime.createJob({
        profile: a.profile,
        input,
        provider: a.mode === "live" ? live.id : "pcl-template",
      });
      pendingJobs.add(job.id);
      try {
        if (files && a.permittedPaths?.length)
          await reviews.registerPorterSource(job.id, {
            files,
            permittedPaths: a.permittedPaths,
          });
      } catch (error) {
        // A rejected grant must not consume shared capacity as an orphan queue.
        pendingJobs.delete(job.id);
        await runtime.cancel(job.id);
        throw error;
      }
      return a.profile === "maker"
        ? admitMakerThreadJob(job, preset, a.deferThreadStart)
        : startCapturedJob(job, preset);
    }
    case "job_read": {
      const { job, summary } = await readJobSnapshot(
        runtime,
        a.jobId,
        hasCurrentJob,
      );
      return {
        summary,
        mode: job.provider === "pcl-template" ? "template" : "live",
        result: job.result,
        ...(job.profile === "maker" || job.profile === "harness"
          ? {
              activity: job.operations.map((op) => ({
                id: op.id,
                name: op.name,
                status: op.status,
                ...(["workspace.read", "harness.read_file"].includes(op.name) &&
                typeof op.path === "string"
                  ? { path: op.path }
                  : {}),
                ...(["maker.report_plan", "harness.report_plan"].includes(
                  op.name,
                ) && op.status === "completed"
                  ? { steps: op.result.steps }
                  : {}),
              })),
            }
          : {}),
        ...(job.profile === "harness"
          ? {
              output: job.providerState?.livePi?.output ?? "",
              ...(await visualPreview(job)),
            }
          : {}),
        error: job.error,
        ...(job.profile === "porter"
          ? { porterSource: await reviews.getPorterSource(job.id) }
          : {}),
        artifacts: job.operations
          .filter(
            (op) => op.status === "completed" && op.result?.outputDirectory,
          )
          .map((op) => ({
            operationId: op.id,
            directory: op.result.outputDirectory,
            files: op.result.artifacts.map((f) =>
              f.path.slice(op.result.outputDirectory.length + 1),
            ),
          })),
        reviews: (
          await fs
            .readdir(
              path.join(
                runtime.store.directory(job.id),
                "review-control/reviews",
              ),
            )
            .catch(() => [])
        )
          .filter((v) => v.endsWith(".json"))
          .map((v) => v.slice(0, -5)),
      };
    }
    case "job_cancel": {
      const cancelled = await runtime.cancel(a.jobId);
      if (!runtime.hasActiveJob(a.jobId)) {
        pendingJobs.delete(a.jobId);
        liveRuns.delete(a.jobId);
      }
      return cancelled;
    }
    case "artifact_read": {
      const { snapshot } = await artifact(a.jobId, a.operationId);
      const bytes = snapshot.files[a.path];
      if (!bytes) throw new Error("Unknown artifact file");
      return {
        path: a.path,
        sha256: sha256(bytes),
        content: a.path.endsWith(".png") ? null : bytes.toString("utf8"),
        binary: a.path.endsWith(".png"),
      };
    }
    case "rh_code_artifact": {
      checkRhOwner(await runtime.getJob(a.jobId), a);
      const { snapshot } = await artifact(a.jobId, a.operationId);
      return {
        files: Object.entries(snapshot.files).map(([name, bytes]) => ({
          path: name,
          sha256: sha256(bytes),
          data: bytes.toString("base64"),
        })),
      };
    }
    case "artifact_directory":
      return { directory: (await artifact(a.jobId, a.operationId)).directory };
    case "maker_review": {
      if (a.kind === "revision")
        return reviews.requestMakerSourceEdit(a.jobId, {
          sourceOperationId: a.operationId,
          request: { operation: "regenerate", spec: a.spec },
        });
      if (a.kind === "restore") {
        const checkpoint = await artifact(a.jobId, a.checkpointOperationId);
        return reviews.requestMakerSourceEdit(a.jobId, {
          sourceOperationId: a.operationId,
          request: {
            operation: "restore",
            checkpointRoot: checkpoint.directory,
          },
        });
      }
      const source = await artifact(a.jobId, a.operationId);
      const request = {
        schema_version: 1,
        edits:
          a.kind === "lock"
            ? []
            : [{ path: a.path, expected_sha256: a.sha256, text: a.text }],
        lock_paths: a.kind === "lock" ? [a.path] : [],
      };
      return reviews.requestMakerSourceEdit(source.job.id, {
        sourceOperationId: a.operationId,
        request,
      });
    }
    case "porter_recipe": {
      if (!["metadata", "identifier"].includes(a.recipe))
        throw new Error("Unknown migration recipe");
      return reviews.requestPorterRecipe(a.jobId, a.recipe);
    }
    case "porter_review":
      return reviews.requestPorterPatch(a.jobId, {
        sourceId: "primary",
        replacements: { [a.path]: a.text },
        purpose: a.purpose,
      });
    case "review_read":
      return reviews.getReview(a.jobId, a.reviewId);
    case "review_apply": {
      const approval = await reviews.approveReview(a.jobId, {
        reviewId: a.reviewId,
        expectedDigest: a.digest,
      });
      return reviews.applyApprovedReview(a.jobId, {
        reviewId: a.reviewId,
        approvalToken: approval.approvalToken,
      });
    }
    case "review_cancel":
      return reviews.revokeReview(a.jobId, { reviewId: a.reviewId });
    case "report_read": {
      const job = await runtime.getJob(a.jobId);
      const report = job.operations.find(
        (op) => op.status === "completed" && op.result?.reportPath,
      )?.result.reportPath;
      if (!report) return null;
      return JSON.parse(
        (await (await runtime.store.workspace(job.id)).read(report)).content,
      );
    }
    case "shutdown":
      closing = true;
      selection = null;
      await runtime.shutdown();
      harnessTools.dispose();
      visualPreviews.clear();
      await Promise.all([
        reviews.shutdown(),
        adapters.shutdown(),
        bridge.shutdown(),
      ]);
      return true;
    default:
      throw new Error("Unknown experimental host operation");
  }
}
// Native calls are serialized, but runtime jobs continue independently. Every
// line receives one bounded envelope; no model or child stdout reaches IPC.
const input = readline.createInterface({
  input: process.stdin,
  crlfDelay: Infinity,
});
for await (const line of input) {
  let reply,
    replyLimit = 1000000;
  try {
    if (Buffer.byteLength(line) > 24 * 1024 * 1024)
      throw new Error("Request exceeds native snapshot limit");
    const request = JSON.parse(line);
    if (request.operation === "rh_code_harness_export")
      replyLimit = 24 * 1024 * 1024;
    if (request.operation !== "rh_code_run" && Buffer.byteLength(line) > 128000)
      throw new Error("Request exceeds 128 KB");
    reply = {
      ok: true,
      result: await dispatch(request.operation, request.args ?? {}),
    };
  } catch (error) {
    reply = { ok: false, error: String(error.message).slice(0, 1000) };
  }
  const output = JSON.stringify(reply);
  process.stdout.write(
    (Buffer.byteLength(output) <= replyLimit
      ? output
      : JSON.stringify({ ok: false, error: "Host response exceeds limit" })) +
      "\n",
  );
  if (closing) break;
}
if (!closing) await dispatch("shutdown", {});
