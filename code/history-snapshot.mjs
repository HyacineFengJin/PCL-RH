/** Native conversation history and saved notes are immutable quoted context.
 * They live in JobStore.files, outside the model's source workspace. A digest
 * binds the complete record (including identity and focused file references),
 * so compact read receipts can reconstruct results without caching user text.
 */
import { check } from "../experimental/runtime/src/errors.mjs";
import { sourcePath, recordDigest, exactHash, readHarnessRecord, writeHarnessRecord } from "./harness-snapshot.mjs";

export const HISTORY_RECORD = "control/harness-history.json";
export const HISTORY_LIMITS = Object.freeze({ messages: 1024, historyBytes: 1_000_000,
  noteCharacters: 8000, noteBytes: 24_000, contextPaths: 20, totalMessages: 4096 });
export const encodedBytes = (value) => Buffer.byteLength(JSON.stringify(value));
const fields = (value, names) => value && typeof value === "object" && !Array.isArray(value) &&
  Object.keys(value).sort().join() === [...names].sort().join();
const text = (value) => typeof value === "string" && value.isWellFormed();
export function boundedExcerpt(value, maximumChars, maximumBytes) {
  let low = 0, high = Math.min(value.length, maximumChars);
  while (low < high) {
    const length = Math.ceil((low + high) / 2);
    const complete = length > 0 && /[\uD800-\uDBFF]/.test(value[length - 1]) ? length - 1 : length;
    if (encodedBytes(value.slice(0, complete)) <= maximumBytes) low = length;
    else high = length - 1;
  }
  if (low > 0 && /[\uD800-\uDBFF]/.test(value[low - 1])) low--;
  return value.slice(0, low);
}
function note(value, label) {
  check(text(value) && Array.from(value).length <= HISTORY_LIMITS.noteCharacters &&
    Buffer.byteLength(value) <= HISTORY_LIMITS.noteBytes && !/[\x00-\x08\x0b-\x1f\x7f-\x9f]/.test(value),
  "INVALID_HARNESS_CONTEXT", `${label} exceeds the native note bounds or contains invalid controls`);
  return value;
}
function checkedHistory(history) {
  check(fields(history, ["schemaVersion", "messages", "startIndex", "totalMessages", "truncated"]) &&
    history.schemaVersion === 1 && Array.isArray(history.messages) && history.messages.length <= HISTORY_LIMITS.messages &&
    Number.isSafeInteger(history.startIndex) && history.startIndex >= 0 &&
    Number.isSafeInteger(history.totalMessages) && history.totalMessages <= HISTORY_LIMITS.totalMessages &&
    history.startIndex + history.messages.length === history.totalMessages &&
    history.truncated === (history.startIndex > 0) && encodedBytes(history) <= HISTORY_LIMITS.historyBytes,
  "INVALID_HARNESS_HISTORY", "Expected the bounded contiguous native history suffix");
  const ids = new Set();
  for (const message of history.messages) {
    check(fields(message, ["id", "role", "content", "jobId"]) && text(message.id) && message.id.length > 0 && encodedBytes(message.id) <= 512 &&
      !ids.has(message.id) && ["user", "assistant", "notice"].includes(message.role) && text(message.content) &&
      Array.from(message.content).length <= 128_000 && (message.jobId === null || (text(message.jobId) && encodedBytes(message.jobId) <= 512)),
    "INVALID_HARNESS_HISTORY", "Invalid or duplicate native history message");
    ids.add(message.id);
  }
  return JSON.parse(JSON.stringify(history));
}

export function captureNativeHistory({ project, conversation, conversationHistory }, manifest = []) {
  check(project && conversation && text(project.id) && project.id.length > 0 && project.id.length <= 128 &&
    text(conversation.id) && conversation.id.length > 0 && conversation.id.length <= 128,
  "INVALID_HARNESS_CONTEXT", "Context requires a captured project and conversation identity");
  const contextPaths = conversation.contextPaths ?? [];
  check(Array.isArray(contextPaths) && contextPaths.length <= HISTORY_LIMITS.contextPaths && new Set(contextPaths).size === contextPaths.length,
    "INVALID_HARNESS_CONTEXT", "Expected at most 20 unique focused source paths");
  const available = new Set(manifest.map((entry) => entry.path));
  for (const name of contextPaths) {
    sourcePath(name);
    check(available.has(name), "CONTEXT_FILE_UNAVAILABLE", "Focused file reference is outside the frozen native source");
  }
  const history = conversationHistory === undefined ? null : checkedHistory(conversationHistory);
  if (history) {
    const recent = conversation.messages ?? [];
    const omitted = conversation.omittedMessages ?? 0;
    check(Array.isArray(recent) && recent.length <= 8 && Number.isSafeInteger(omitted) && omitted >= 0 &&
      omitted + recent.length === history.totalMessages,
    "INVALID_HARNESS_HISTORY", "Recent projection differs from the captured native history scope");
    const overlapping = Math.min(recent.length, history.messages.length);
    const suffix = history.messages.slice(-overlapping || history.messages.length);
    check(recent.slice(-overlapping || recent.length).every((message, index) => message?.role === suffix[index]?.role && text(message.content) &&
      suffix[index].content.startsWith(message.content)),
    "INVALID_HARNESS_HISTORY", "Recent projection differs from the captured native history messages");
  }
  const record = { schemaVersion: 1, projectId: project.id, conversationId: conversation.id,
    instructions: note(project.instructions ?? "", "Project instructions"),
    contextNote: note(conversation.contextNote ?? "", "Conversation note"), contextPaths: [...contextPaths], conversationHistory: history };
  return { record, descriptor: historyDescriptor(record) };
}
export function historyDescriptor(record) {
  const history = record.conversationHistory;
  return { schemaVersion: 1, controlDigest: recordDigest(record), available: history !== null,
    messageCount: history?.messages.length ?? 0, startIndex: history?.startIndex ?? null,
    totalMessages: history?.totalMessages ?? null, truncated: history?.truncated ?? null,
    contextPathsCount: record.contextPaths.length };
}
export async function stageNativeHistory(store, job, snapshot) {
  await writeHarnessRecord(await store.files(job.id), HISTORY_RECORD, snapshot.record);
}
export async function readNativeHistory(snapshot) {
  const descriptor = snapshot.harness.history;
  if (descriptor === undefined) return null; // Jobs created before context capture have no retrievable history.
  check(fields(descriptor, ["schemaVersion", "controlDigest", "available", "messageCount", "startIndex", "totalMessages", "truncated", "contextPathsCount"]) &&
    descriptor.schemaVersion === 1 && exactHash(descriptor.controlDigest),
  "HISTORY_SNAPSHOT_CHANGED", "Invalid immutable context descriptor");
  const record = await readHarnessRecord(snapshot.control, HISTORY_RECORD);
  check(fields(record, ["schemaVersion", "projectId", "conversationId", "instructions", "contextNote", "contextPaths", "conversationHistory"]) &&
    record.schemaVersion === 1 && record.projectId === snapshot.harness.projectId && record.conversationId === snapshot.harness.conversationId &&
    recordDigest(record) === descriptor.controlDigest,
  "HISTORY_SNAPSHOT_CHANGED", "The immutable native context changed or belongs to another conversation");
  // Reuse admission validation after checking the original digest; parsing an
  // altered control file must never quietly normalize it into a new snapshot.
  const checked = captureNativeHistory({ project: { id: record.projectId, instructions: record.instructions },
    conversation: { id: record.conversationId, contextNote: record.contextNote, contextPaths: record.contextPaths,
      messages: [], omittedMessages: record.conversationHistory?.totalMessages ?? 0 },
    ...(record.conversationHistory === null ? {} : { conversationHistory: record.conversationHistory }) }, snapshot.record.manifest);
  check(recordDigest(checked.descriptor) === recordDigest(descriptor), "HISTORY_SNAPSHOT_CHANGED", "Immutable context scope differs from admission");
  return record;
}
