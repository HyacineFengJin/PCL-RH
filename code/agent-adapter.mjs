/** Native code freezes source before preparation. This adapter creates a
 * generic project job, stages only supplied bytes, and defers the provider until
 * Rust durably binds the captured project and conversation. */
import { agentContext, fitHarnessInput } from "./agent-context.mjs";
import { captureNativeSnapshot, stageNativeSnapshot, decodeText } from "./harness-snapshot.mjs";
import { captureNativeHistory, stageNativeHistory } from "./history-snapshot.mjs";
import { captureHarnessToolScope } from "./harness-tool-catalog.mjs";

export async function prepareCodeJob(a, { runtime, preset, provider, jobs, admit }) {
  if (!preset) throw new Error("Save and select an AI preset first");
  if (jobs.filter((job) => ["queued", "running"].includes(job.status)).length >= 2)
    throw new Error("The shared runtime already has two active jobs");
  const toolScope = captureHarnessToolScope(a.enabledPlugins, a.mode);
  const snapshot = captureNativeSnapshot(a.sourceFiles ?? [], a.protectedPaths ?? []);
  const history = captureNativeHistory(a, snapshot.record.manifest);
  const context = agentContext({ ...a,
    historyRecord: history.record,
    source: Object.fromEntries([...snapshot.files].map(([path, bytes]) => [path, decodeText(bytes)])),
    sourceManifest: snapshot.record.manifest,
    protectedPaths: snapshot.record.protectedPaths,
    sourceOmissions: a.sourceOmissions, sourceOmissionCount: a.sourceOmissionCount });
  const job = await runtime.createJob({
    profile: "harness", provider,
    input: fitHarnessInput({
      rhCode: { projectId: a.project.id, conversationId: a.conversation.id },
      prompt: context.prompt,
      harness: {
        schemaVersion: 1, mode: a.mode, pluginId: "rh.project",
        projectId: a.project.id, conversationId: a.conversation.id,
        thread: context.thread, source: snapshot.descriptor, history: history.descriptor, ...toolScope,
      },
    }),
  });
  try {
    await stageNativeSnapshot(runtime.store, job, snapshot);
    await stageNativeHistory(runtime.store, job, history);
    const { retained, omitted, method } = job.input.harness.thread.projection;
    return { ...(await admit(job, preset, true)), contextProjection: { retained, omitted, method } };
  } catch (error) {
    await runtime.cancel(job.id);
    throw error;
  }
}
