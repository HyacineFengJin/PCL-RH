/** Native source bytes are copied once, before provider admission. The private
 * manifest preserves the original hashes: neither reads nor proposals reconcile
 * or rewrite a user's files or an existing Maker ownership receipt. */
import { createHash, randomUUID } from "node:crypto";
import { constants } from "node:fs";
import * as fs from "node:fs/promises";
import { check } from "../experimental/runtime/src/errors.mjs";

export const HARNESS_LIMITS = Object.freeze({
  files: 1000,
  fileBytes: 1024 * 1024,
  sourceBytes: 16 * 1024 * 1024,
  recordBytes: 2_000_000,
});
export const SNAPSHOT_RECORD = "control/harness-snapshot.json";
export const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
export function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object")
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  return JSON.stringify(value);
}
export const recordDigest = (record) => sha256(canonical(record));
export const exactHash = (value) => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
// Rust's String ordering compares UTF-8 bytes. Avoid localeCompare so native
// code and the runtime reproduce one source fingerprint on every machine.
export const compareSourcePaths = (a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b));
// Match the native Harness source_path boundary before staging or review.
const excludedSegments = new Set(["node_modules", "target", "build", "dist", "vendor", "venv", "__pycache__"]);

export function sourcePath(value) {
  check(
    typeof value === "string" && value.isWellFormed() && value.length > 0 && Buffer.byteLength(value) <= 512 &&
      !/[\\:\x00-\x1f\x7f-\x9f]/.test(value) &&
      value.split("/").every((part) => part && !part.startsWith(".") && !excludedSegments.has(part)),
    "INVALID_SOURCE_PATH", "Expected a visible project source path outside generated or dependency directories",
  );
  return value;
}
export function decodeText(bytes) {
  let text;
  try { text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes); }
  catch { check(false, "INVALID_SOURCE_TEXT", "The frozen source must contain UTF-8 text"); }
  check(!text.includes("\0"), "INVALID_SOURCE_TEXT", "Harness source text cannot contain NUL");
  return text;
}

function ownershipProtection(files) {
  const receipt = files.get("creator-ownership.json");
  if (!receipt) return [];
  // Legacy Maker locks remain meaningful in a generic project. A changed hash
  // is handwritten content, even if the older receipt still calls it a template.
  // Malformed receipts fail closed for edits while allowing inspection/planning.
  let records;
  try {
    const parsed = JSON.parse(decodeText(receipt));
    check(parsed.schema_version === 1 && parsed.files && typeof parsed.files === "object" && !Array.isArray(parsed.files), "INVALID_OWNERSHIP", "Unsupported ownership receipt");
    records = parsed.files;
    check(Object.keys(records).length === files.size - 1 &&
      Object.entries(records).every(([name, record]) => name !== "creator-ownership.json" && files.has(name) &&
        record && typeof record === "object" && !Array.isArray(record) &&
        ["template", "locked", "user"].includes(record.owner) && exactHash(record.sha256)),
    "INVALID_OWNERSHIP", "Ownership inventory differs from captured source");
  } catch { return [...files.keys()].sort(compareSourcePaths); }
  const protectedPaths = ["creator-ownership.json"];
  for (const [name, bytes] of files) {
    if (name === "creator-ownership.json") continue;
    const record = Object.hasOwn(records, name) ? records[name] : null;
    if (!record || !["template", "locked", "user"].includes(record.owner) ||
      record.owner === "locked" || record.sha256 !== sha256(bytes)) protectedPaths.push(name);
  }
  return protectedPaths.sort(compareSourcePaths);
}

export function captureNativeSnapshot(entries = [], protectedPaths = []) {
  check(Array.isArray(entries) && entries.length <= HARNESS_LIMITS.files,
    "INVALID_SOURCE_SNAPSHOT", "Native source snapshot exceeds its file limit");
  const files = new Map();
  let totalBytes = 0;
  for (const entry of entries) {
    check(entry && typeof entry === "object", "INVALID_SOURCE_SNAPSHOT", "Invalid native source snapshot entry");
    sourcePath(entry.path);
    check(!files.has(entry.path) && typeof entry.data === "string" &&
      entry.data.length <= Math.ceil(HARNESS_LIMITS.fileBytes / 3) * 4 &&
      /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(entry.data),
    "INVALID_SOURCE_SNAPSHOT", "Invalid or duplicate native snapshot content");
    const bytes = Buffer.from(entry.data, "base64");
    totalBytes += bytes.length;
    check(bytes.length <= HARNESS_LIMITS.fileBytes && totalBytes <= HARNESS_LIMITS.sourceBytes &&
      exactHash(entry.sha256) && sha256(bytes) === entry.sha256 &&
      (entry.size === undefined || entry.size === bytes.length),
    "INVALID_SOURCE_SNAPSHOT", "Native source snapshot failed hash or size verification");
    decodeText(bytes);
    files.set(entry.path, bytes);
  }
  check(Array.isArray(protectedPaths) && protectedPaths.length <= HARNESS_LIMITS.files,
    "INVALID_SOURCE_SNAPSHOT", "Invalid source protection inventory");
  const locked = new Set(ownershipProtection(files));
  for (const name of protectedPaths) {
    sourcePath(name);
    check(files.has(name), "INVALID_SOURCE_SNAPSHOT", "Protected path is outside the frozen snapshot");
    locked.add(name);
  }
  const manifest = [...files].map(([path, bytes]) => ({ path, sha256: sha256(bytes), size: bytes.length }))
    .sort((a, b) => compareSourcePaths(a.path, b.path));
  for (const entry of manifest) {
    const parts = entry.path.split("/");
    for (let length = 1; length < parts.length; length++)
      check(!files.has(parts.slice(0, length).join("/")), "INVALID_SOURCE_SNAPSHOT", "Source file conflicts with a directory path");
  }
  const record = { schemaVersion: 1, manifest, protectedPaths: [...locked].sort(compareSourcePaths) };
  return { files, record, descriptor: { directory: "source", fingerprint: recordDigest(manifest),
    controlDigest: recordDigest(record), fileCount: manifest.length, totalBytes } };
}

// Workspace's older domain artifacts have a decimal MB limit. RH Code accepts
// the native editor's 1 MiB files, using the same nofollow path checks and fresh
// publication without broadening another workflow's storage contract.
async function writePrivateBytes(workspace, relative, bytes, maxBytes) {
  check(bytes instanceof Uint8Array && bytes.length <= maxBytes,
    "INVALID_SOURCE_CONTENT", "Private Harness file exceeds its byte limit");
  const target = await workspace.resolve(relative, { parents: true });
  const temporary = `${target}.${randomUUID()}.tmp`;
  let published = false;
  try {
    const handle = await fs.open(temporary, "wx", 0o600);
    try { await handle.writeFile(bytes); await handle.sync(); }
    finally { await handle.close(); }
    // link() publishes without replacing an existing proposal or snapshot file.
    try { await fs.link(temporary, target); published = true; }
    catch (error) {
      if (error.code !== "EEXIST") throw error;
      // A crash may leave exact staged bytes before the immutable review record
      // is published. Retrying that operation accepts only the identical file.
      await readPrivateBytes(workspace, relative, { size: bytes.length, sha256: sha256(bytes) }, maxBytes);
    }
  } finally { await fs.rm(temporary, { force: true }); }
  return published;
}

async function readPrivateBytes(workspace, relative, expected, maxBytes) {
  const target = await workspace.resolve(relative);
  const handle = await fs.open(target, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = await handle.stat();
    check(stat.isFile() && stat.nlink <= 1 && stat.size <= maxBytes,
      "UNSAFE_SOURCE", "Expected a private regular source file");
    // Allocate from the checked size and one extra byte. A concurrent growth
    // must fail validation rather than turn readFile() into an unbounded read.
    const buffer = Buffer.allocUnsafe(stat.size + 1);
    let length = 0;
    while (length < buffer.length) {
      const part = await handle.read(buffer, length, buffer.length - length, length);
      if (part.bytesRead === 0) break;
      length += part.bytesRead;
    }
    check(length === stat.size, "SOURCE_SNAPSHOT_CHANGED", "Private snapshot size changed during read");
    const bytes = buffer.subarray(0, length);
    if (expected) check(bytes.length === expected.size && sha256(bytes) === expected.sha256,
      "SOURCE_SNAPSHOT_CHANGED", "The immutable private snapshot changed");
    return bytes;
  } finally { await handle.close(); }
}

export const writeHarnessBytes = (workspace, relative, bytes) =>
  writePrivateBytes(workspace, relative, bytes, HARNESS_LIMITS.fileBytes);
export const readSnapshotBytes = (workspace, relative, expected) =>
  readPrivateBytes(workspace, relative, expected, HARNESS_LIMITS.fileBytes);
// Binary build inputs have a separate native capture budget. They remain outside
// the model workspace and never widen the source editor's text contract.
export const writeHarnessBuildBytes = (workspace, relative, bytes) =>
  writePrivateBytes(workspace, relative, bytes, 4 * 1024 * 1024);
export const readHarnessBuildBytes = (workspace, relative, expected) =>
  readPrivateBytes(workspace, relative, expected, 4 * 1024 * 1024);
export async function writeHarnessRecord(workspace, relative, record) {
  return writePrivateBytes(workspace, relative, Buffer.from(JSON.stringify(record)), HARNESS_LIMITS.recordBytes);
}
export async function readHarnessRecord(workspace, relative) {
  return JSON.parse(decodeText(await readPrivateBytes(workspace, relative, null, HARNESS_LIMITS.recordBytes)));
}

export async function stageNativeSnapshot(store, job, snapshot) {
  const workspace = await store.workspace(job.id);
  for (const [name, bytes] of snapshot.files) {
    await writeHarnessBytes(workspace, `source/${name}`, bytes);
    await readSnapshotBytes(workspace, `source/${name}`, { size: bytes.length, sha256: sha256(bytes) });
  }
  const control = await store.files(job.id);
  await writeHarnessRecord(control, SNAPSHOT_RECORD, snapshot.record);
}
