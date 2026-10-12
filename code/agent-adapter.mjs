/** Native code freezes source before preparation. This adapter creates a
 * generic project job, stages only supplied bytes, and defers the provider until
 * Rust durably binds the captured project and conversation. */
import { agentContext } from "./agent-context.mjs";
import { captureNativeSnapshot, stageNativeSnapshot, decodeText } from "./harness-snapshot.mjs";

export async function prepareCodeJob(a, { runtime, preset, provider, jobs, admit }) {
  if (!preset) throw new Error("Save and select an AI preset first");
  if (jobs.filter((job) => ["queued", "running"].includes(job.status)).length >= 2)
    throw new Error("The shared runtime already has two active jobs");
  const snapshot = captureNativeSnapshot(a.sourceFiles ?? [], a.protectedPaths ?? []);
  const context = agentContext({ ...a,
    source: Object.fromEntries([...snapshot.files].map(([path, bytes]) => [path, decodeText(bytes)])),
    sourceManifest: snapshot.record.manifest,
    protectedPaths: snapshot.record.protectedPaths,
    sourceOmissions: a.sourceOmissions, sourceOmissionCount: a.sourceOmissionCount });
  const job = await runtime.createJob({
    profile: "harness", provider,
    input: {
      rhCode: { projectId: a.project.id, conversationId: a.conversation.id },
      prompt: context.prompt,
      harness: {
        schemaVersion: 1, mode: a.mode, pluginId: "rh.project",
        projectId: a.project.id, conversationId: a.conversation.id,
        thread: context.thread, source: snapshot.descriptor,
      },
    },
  });
  try {
    await stageNativeSnapshot(runtime.store, job, snapshot);
    return { ...(await admit(job, preset, true)), contextProjection: context.projection };
  } catch (error) {
    await runtime.cancel(job.id);
    throw error;
  }
}
