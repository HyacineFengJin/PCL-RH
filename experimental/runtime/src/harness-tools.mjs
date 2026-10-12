/** Generic project tools expose only a host-captured source snapshot and review
 * proposals. The injected service owns snapshot identity, source hashes and
 * proposal persistence; this registry never discovers files or grants access.
 * Project plugins can add their own domain tools without changing this contract.
 */
import { check, throwIfAborted } from "./errors.mjs";

const TEXT_BYTES = 1_048_576;
const PROPOSAL_BYTES = 1_800_000;
// Keep model-visible validation aligned with native capture/adoption policy.
const excludedSegments = new Set(["node_modules", "target", "build", "dist", "vendor", "venv", "__pycache__"]);
function object(value, keys) {
  check(
    value && typeof value === "object" && !Array.isArray(value) &&
      Object.keys(value).every((key) => keys.includes(key)),
    "INVALID_ARGUMENTS", "Expected an object with known fields",
  );
}
function text(value, maximum, label) {
  check(typeof value === "string" && value.trim().length > 0 &&
    value.length <= maximum, "INVALID_ARGUMENTS", `Invalid ${label}`);
}
function relative(value, { prefix = false } = {}) {
  if (prefix && value === "") return;
  check(typeof value === "string" && value.isWellFormed() && value.length > 0 && Buffer.byteLength(value) <= 512 &&
    !/^[\\/]/.test(value) && !/[\\:\x00-\x1f\x7f-\x9f]/.test(value),
  "UNSAFE_PATH", "Expected a canonical project-relative path");
  const parts = (prefix ? value.replace(/\/$/, "") : value).split("/");
  check(parts.every((part) => part && !part.startsWith(".") && !excludedSegments.has(part)),
    "UNSAFE_PATH", "Hidden, generated and dependency path segments are unavailable");
}
function optionalInteger(value, min, max, label) {
  if (value === undefined) return;
  check(Number.isSafeInteger(value) && value >= min && value <= max,
    "INVALID_ARGUMENTS", `Invalid ${label}`);
}
const schemas = {
  list_files: {
    properties: {
      prefix: { type: "string", maxLength: 512 },
      offset: { type: "integer", minimum: 0, maximum: 1000 },
      limit: { type: "integer", minimum: 1, maximum: 100 },
    },
    validate(args) {
      object(args, ["prefix", "offset", "limit"]);
      if (args.prefix !== undefined) relative(args.prefix, { prefix: true });
      optionalInteger(args.offset, 0, 1000, "listing offset");
      optionalInteger(args.limit, 1, 100, "listing limit");
    },
  },
  read_file: {
    properties: {
      path: { type: "string", maxLength: 512 },
      offset: { type: "integer", minimum: 0, maximum: TEXT_BYTES },
      limit: { type: "integer", minimum: 1, maximum: 16_000 },
    },
    required: ["path"],
    validate(args) {
      object(args, ["path", "offset", "limit"]); relative(args.path);
      optionalInteger(args.offset, 0, TEXT_BYTES, "read offset");
      optionalInteger(args.limit, 1, 16_000, "read limit");
    },
  },
  search_files: {
    properties: {
      query: { type: "string", minLength: 1, maxLength: 200 },
      pathPrefix: { type: "string", maxLength: 512 },
      limit: { type: "integer", minimum: 1, maximum: 100 },
    },
    required: ["query"],
    validate(args) {
      object(args, ["query", "pathPrefix", "limit"]);
      text(args.query, 200, "literal search query");
      check(!args.query.includes("\n"), "INVALID_ARGUMENTS", "Search one literal line at a time");
      if (args.pathPrefix !== undefined) relative(args.pathPrefix, { prefix: true });
      optionalInteger(args.limit, 1, 100, "search limit");
    },
  },
  read_context: {
    properties: { pathOffset: { type: "integer", minimum: 0, maximum: 20 } },
    validate(args) { object(args, ["pathOffset"]); optionalInteger(args.pathOffset, 0, 20, "context path offset"); },
  },
  read_history: {
    properties: {
      offset: { type: "integer", minimum: 0, maximum: 1024 },
      limit: { type: "integer", minimum: 1, maximum: 50 },
      contentOffset: { type: "integer", minimum: 0, maximum: 256_000,
        description: "UTF-16 code-unit offset within one message; requires limit=1. Use returned nextContentOffset to continue. An offset inside a surrogate pair rounds back to the complete character." },
    },
    allOf: [{ if: { required: ["contentOffset"] }, then: { required: ["limit"], properties: { limit: { const: 1 } } } }],
    validate(args) {
      object(args, ["offset", "limit", "contentOffset"]);
      optionalInteger(args.offset, 0, 1024, "history offset");
      optionalInteger(args.limit, 1, 50, "history page limit");
      optionalInteger(args.contentOffset, 0, 256_000, "history content offset");
      check(args.contentOffset === undefined || args.limit === 1, "INVALID_ARGUMENTS", "History contentOffset requires limit=1");
    },
  },
  search_history: {
    properties: {
      query: { type: "string", minLength: 1, maxLength: 200 },
      limit: { type: "integer", minimum: 1, maximum: 100 },
    },
    required: ["query"],
    validate(args) {
      object(args, ["query", "limit"]);
      check(typeof args.query === "string" && args.query.isWellFormed() && args.query.length > 0 && args.query.length <= 200,
        "INVALID_ARGUMENTS", "Use a literal history query of at most 200 characters");
      optionalInteger(args.limit, 1, 100, "history search limit");
    },
  },
  report_plan: {
    properties: {
      summary: { type: "string", minLength: 1, maxLength: 2000 },
      steps: { type: "array", minItems: 1, maxItems: 40,
        items: { type: "string", minLength: 1, maxLength: 1000 } },
    },
    required: ["summary", "steps"],
    validate(args) {
      object(args, ["summary", "steps"]); text(args.summary, 2000, "plan summary");
      check(Array.isArray(args.steps) && args.steps.length > 0 && args.steps.length <= 40,
        "INVALID_ARGUMENTS", "Provide 1–40 intended steps");
      args.steps.forEach((step) => text(step, 1000, "plan step"));
      check(Buffer.byteLength(JSON.stringify(args)) <= 16_000,
        "INVALID_ARGUMENTS", "Plan exceeds its encoded input limit");
    },
  },
  propose_changes: {
    properties: {
      summary: { type: "string", minLength: 1, maxLength: 2000 },
      changes: {
        type: "array", minItems: 1, maxItems: 1000,
        items: {
          type: "object",
          properties: {
            path: { type: "string", maxLength: 512 },
            expected_sha256: { anyOf: [{ type: "string", pattern: "^[a-f0-9]{64}$" }, { type: "null" }] },
            content: { anyOf: [{ type: "string", maxLength: TEXT_BYTES }, { type: "null" }] },
          },
          required: ["path", "expected_sha256", "content"], additionalProperties: false,
        },
      },
    },
    required: ["summary", "changes"],
    validate(args) {
      object(args, ["summary", "changes"]); text(args.summary, 2000, "proposal summary");
      check(Array.isArray(args.changes) && args.changes.length > 0 && args.changes.length <= 1000,
        "INVALID_ARGUMENTS", "Propose 1–1000 changes");
      const seen = new Set();
      for (const change of args.changes) {
        object(change, ["path", "expected_sha256", "content"]); relative(change.path);
        check(!seen.has(change.path), "INVALID_ARGUMENTS", "Duplicate proposal path");
        seen.add(change.path);
        check(change.expected_sha256 === null ||
          (typeof change.expected_sha256 === "string" && /^[a-f0-9]{64}$/.test(change.expected_sha256)),
        "INVALID_ARGUMENTS", "Use the original SHA-256 or null for a new file");
        check(change.content === null || (typeof change.content === "string" && change.content.isWellFormed() && !change.content.includes("\0")),
          "INVALID_ARGUMENTS", "Provide UTF-8 text without NUL, or null to delete an existing file");
        check(change.content !== null || change.expected_sha256 !== null,
          "INVALID_ARGUMENTS", "A deletion needs an existing file hash");
        check(Buffer.byteLength(change.content ?? "") <= TEXT_BYTES,
          "INVALID_ARGUMENTS", "Proposed file exceeds 1 MiB");
      }
      check(Buffer.byteLength(JSON.stringify(args)) <= PROPOSAL_BYTES,
        "INVALID_ARGUMENTS", "Proposal exceeds its encoded input limit");
    },
  },
};
const definitions = [
  ["list_files", "listFiles", "List a bounded page of files from the captured source manifest. Optional prefix narrows paths; continue with the returned nextOffset."],
  ["read_file", "readFile", "Read a bounded text excerpt from a captured project file and return its original SHA-256. Offset and limit count characters."],
  ["search_files", "searchFiles", "Search captured project text for a literal query and return bounded matching lines."],
  ["read_context", "readContext", "Read saved project instructions, the conversation note and focused file references as quoted context. Results identify truncation and captured history scope; continue focused references with nextPathOffset as pathOffset when present. This tool grants no permissions."],
  ["read_history", "readHistory", "Read a bounded page from this job's captured native history suffix. Offset is relative to the captured suffix; results include absolute message indices, stable IDs, roles and job IDs. To read a long message fully, use limit=1 and pass its nextContentOffset as contentOffset while keeping offset unchanged. Content offsets count UTF-16 code units and round back inside a surrogate pair. Older upstream messages may be unavailable."],
  ["search_history", "searchHistory", "Search captured native conversation content for a literal query. Return bounded snippets, absolute message indices and stable IDs. Results identify older upstream messages outside this snapshot."],
  ["report_plan", "reportPlan", "Record a plan of intended work. This receipt does not prove any step was completed."],
  ["propose_changes", "proposeChanges", "Create a pending source-change proposal for separate user review. Supply original hashes for existing files, null for new files and null content for deletions. Use visible source paths outside generated/dependency directories and UTF-8 text without NUL. This tool never modifies original source."],
];
function inspectionReceipt(name, result) {
  if (["read_context", "read_history", "search_history"].includes(name)) {
    const base = { schemaVersion: 1, label: "FROZEN_NATIVE_CONTEXT_INSPECTION", contentStored: false,
      controlDigest: result.controlDigest, scope: result.scope };
    if (name === "read_context") return { ...base, contextAvailable: result.contextAvailable,
      projectInstructionCharacters: result.projectInstructions?.content.length ?? 0,
      contextNoteCharacters: result.contextNote?.content.length ?? 0, contextPathsCount: result.contextPaths.length,
      pathOffset: result.pathOffset ?? 0, nextPathOffset: result.nextPathOffset ?? null,
      truncated: !!(result.projectInstructions?.contentTruncated || result.contextNote?.contentTruncated || result.contextPathsTruncated) };
    if (name === "read_history") return { ...base, offset: result.offset, nextOffset: result.nextOffset,
      messageCount: result.messages.length, charactersRead: result.messages.reduce((total, message) => total + message.content.length, 0),
      ...(result.messages.length === 1 ? { contentOffset: result.messages[0].contentOffset, nextContentOffset: result.messages[0].nextContentOffset } : {}),
      truncated: !!result.truncated };
    return { ...base, matchCount: result.matches.length, truncated: !!result.truncated };
  }
  const base = { schemaVersion: 1, label: "FROZEN_SOURCE_INSPECTION", contentStored: false };
  if (name === "read_file") return { ...base, path: result.path, sha256: result.sha256, size: result.size,
    offset: result.offset, nextOffset: result.nextOffset, charactersRead: result.content.length,
    protected: !!result.protected, truncated: !!result.truncated };
  if (name === "list_files") return { ...base, fileCount: result.files.length, total: result.total, nextOffset: result.nextOffset };
  return { ...base, matchCount: result.matches.length, truncated: !!result.truncated };
}

export function registerHarnessTools(registry, service) {
  check(service && definitions.every(([, method]) => typeof service[method] === "function"),
    "INVALID_HARNESS_SERVICE", "Supply the host-owned harness service");
  for (const [name, method, description] of definitions) {
    const { properties, required = [], validate, allOf } = schemas[name];
    registry.register({
      name: `harness.${name}`, description,
      owner: ["read_context", "read_history", "search_history"].includes(name) ? "rh.conversation" :
        ["report_plan", "propose_changes"].includes(name) ? "rh.changes" : "rh.files",
      modes: name === "propose_changes" ? ["execute"] : name === "report_plan" ? ["plan", "execute"] : ["ask", "plan", "execute"],
      capability: ["read_context", "read_history", "search_history"].includes(name) ? "conversation.history" :
        ["report_plan", "propose_changes"].includes(name) ? "project.review" : "project.files.read",
      replaySafety: ["report_plan", "propose_changes"].includes(name) ? "idempotent" : "read-only",
      inputSchema: { type: "object", properties, required, additionalProperties: false, ...(allOf ? { allOf } : {}) },
      validate,
      // Inspection text already lives in the immutable source/context snapshot. Durable
      // receipts retain scope/hash/counts; same-ID replay verifies that snapshot
      // again and reconstructs the model result instead of caching source text.
      ...(!["report_plan", "propose_changes"].includes(name) ? {
        replayCompleted: true,
        projectReceipt: (result) => inspectionReceipt(name, result),
      } : {}),
      execute(args, ctx) {
        throwIfAborted(ctx.signal);
        return service[method](ctx, args);
      },
    });
  }
  return registry;
}
