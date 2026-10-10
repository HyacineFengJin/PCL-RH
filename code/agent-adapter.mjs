/** Cordis-facing RH Agent adapter. Native code freezes source first; the model
 * operates in a job copy and every source edit keeps the existing review gate.
 * Preparation never invokes the provider. Start follows durable native binding. */
import { randomUUID, createHash } from "node:crypto";
import { agentContext } from "./agent-context.mjs";
import { reconcileNativeOwnership } from "./source-ownership.mjs";
import { snapshotProtection } from "../experimental/runtime/vendor/maker/protection.mjs";
const digest = (b) => createHash("sha256").update(b).digest("hex");
export async function prepareCodeJob(
  a,
  { runtime, preset, provider, jobs, admit },
) {
  if (!preset) throw new Error("Save and select an AI preset first");
  if (jobs.filter((j) => ["queued", "running"].includes(j.status)).length >= 2)
    throw new Error("The shared runtime already has two active jobs");
  const context = agentContext(a);
  const files = Object.create(null);
  let total = 0;
  for (const entry of a.sourceFiles ?? []) {
    if (
      typeof entry.path !== "string" ||
      entry.path.split(/[\\/]/).some((p) => !p || p.startsWith(".")) ||
      entry.path.startsWith("/") ||
      Object.hasOwn(files, entry.path)
    )
      throw new Error("Invalid native source snapshot");
    const bytes = Buffer.from(entry.data, "base64");
    total += bytes.length;
    if (
      digest(bytes) !== entry.sha256 ||
      total > 256000 ||
      Object.keys(files).length >= 300
    )
      throw new Error("Native source snapshot failed verification");
    files[entry.path] = bytes;
  }
  let spec = files["creator-spec.json"]
    ? JSON.parse(files["creator-spec.json"].toString("utf8"))
    : null;
  if (a.mode === "execute" && Object.keys(files).length && !spec)
    throw new Error(
      "This source uses no supported Maker adapter. Ask and Plan are available; source generation currently supports Fabric 1.21.1 Maker projects.",
    );
  if (
    a.mode === "execute" &&
    (a.target.minecraft !== "1.21.1" || !a.target.loaders.includes("Fabric"))
  )
    throw new Error(
      "The current source-generation adapter supports Fabric 1.21.1. Other targets can be discussed and planned.",
    );
  spec ??= {
    schema_version: 1,
    target: "fabric-1.21.1",
    mod_id: "rh_mod",
    name: a.project.name,
    description: "RH Code project",
    items: [
      {
        id: "rh_item",
        names: { zh_cn: "RH 物品", en_us: "RH Item" },
        color: "#8160b0",
        max_count: 64,
        recipe: { ingredients: ["minecraft:paper"], count: 1 },
      },
    ],
  };
  const manualChanges = reconcileNativeOwnership(files);
  context.thread.protectedPaths = snapshotProtection({ files });
  const canEdit = !!files["creator-spec.json"];
  const reviewId = randomUUID(),
    operationId = `host-${reviewId}`,
    outputDirectory = `reviewed/${reviewId}`;
  const inventory = Object.entries(files).map(([path, bytes]) => ({
    path,
    sha256: digest(bytes),
    expected_sha256: digest(bytes),
    size: bytes.length,
  }));
  const job = await runtime.createJob({
    profile: "maker",
    provider,
    input: {
      rhCode: { projectId: a.project.id, conversationId: a.conversation.id },
      spec,
      prompt: context.prompt,
      makerThread: context.thread,
      ...(canEdit
        ? {
            sourceOperationId: operationId,
            sourceDirectory: outputDirectory,
            sourceInventory: inventory,
          }
        : {}),
    },
  });
  try {
    if (canEdit) {
      const workspace = await runtime.store.workspace(job.id),
        artifacts = [];
      for (const [name, bytes] of Object.entries(files)) {
        const written = await workspace.writeBytes(
          `${outputDirectory}/${name}`,
          bytes,
        );
        artifacts.push({ ...written, sha256: digest(bytes) });
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
          sourceOrigin: "native-project-snapshot",
          reconciledManualSource: manualChanges,
        },
      });
      await runtime.store.save(job);
    }
    return {
      ...(await admit(job, preset, true)),
      contextProjection: context.projection,
    };
  } catch (error) {
    await runtime.cancel(job.id);
    throw error;
  }
}
