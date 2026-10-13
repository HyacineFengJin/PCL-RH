/** Native build authority and binary assets are frozen before admission. Model
 * tools can choose a fixed recipe and an existing proposal, never new authority,
 * filesystem roots, executable paths or uncaptured binary inputs. */
import { check, throwIfAborted } from "../experimental/runtime/src/errors.mjs";
import {
  HARNESS_LIMITS, sourcePath, exactHash, sha256, recordDigest, compareSourcePaths,
  writeHarnessRecord, readHarnessRecord, writeHarnessBuildBytes, readHarnessBuildBytes,
} from "./harness-snapshot.mjs";

export const BUILD_LIMITS = Object.freeze({ binaryBytes: 4 * 1024 * 1024, attempts: 20,
  timeoutMs: 600_000, outputBytes: 2 * 1024 * 1024, artifacts: 100,
  artifactBytes: 16 * 1024 * 1024, artifactTotalBytes: 64 * 1024 * 1024 });
const RECORD = "control/harness-build-input.json";
const binaryPath = (name) => {
  sourcePath(name);
  const segments = name.split("/");
  check(segments.slice(0, -1).includes("src") || name === "gradle/wrapper/gradle-wrapper.jar",
    "INVALID_BUILD_INPUT", "Binary build inputs must be captured source assets or the Gradle wrapper JAR");
};
export function captureBuildInput(policy, entries = [], textManifest = []) {
  const buildPolicy = policy === undefined ? { enabled: false, network: false } : policy;
  check(buildPolicy && Object.keys(buildPolicy).sort().join() === "enabled,network" &&
    typeof buildPolicy.enabled === "boolean" && typeof buildPolicy.network === "boolean",
  "INVALID_BUILD_POLICY", "Native build policy requires explicit enabled and network flags");
  check(Array.isArray(entries) && entries.length <= HARNESS_LIMITS.files,
    "INVALID_BUILD_INPUT", "Binary build input inventory exceeded its limit");
  const files = new Map(), names = new Set(textManifest.map((entry) => entry.path));
  let totalBytes = 0;
  for (const entry of entries) {
    check(entry && Object.keys(entry).sort().join() === "base64,path,sha256,size",
      "INVALID_BUILD_INPUT", "Expected exact native binary build fields");
    binaryPath(entry.path);
    check(!names.has(entry.path) && !files.has(entry.path) && typeof entry.base64 === "string" &&
      entry.base64.length <= Math.ceil(BUILD_LIMITS.binaryBytes / 3) * 4 &&
      /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(entry.base64),
    "INVALID_BUILD_INPUT", "Duplicate or invalid native binary build content");
    const bytes = Buffer.from(entry.base64, "base64");
    totalBytes += bytes.length;
    check(Number.isSafeInteger(entry.size) && entry.size === bytes.length &&
      bytes.length <= HARNESS_LIMITS.fileBytes && totalBytes <= BUILD_LIMITS.binaryBytes && exactHash(entry.sha256) && sha256(bytes) === entry.sha256,
    "INVALID_BUILD_INPUT", "Native binary build bytes failed size or hash validation");
    files.set(entry.path, bytes);
  }
  const allNames = new Set([...names, ...files.keys()]);
  for (const name of allNames) {
    const parts = name.split("/");
    for (let depth = 1; depth < parts.length; depth++)
      check(!allNames.has(parts.slice(0, depth).join("/")), "INVALID_BUILD_INPUT", "Build files conflict with a directory path");
  }
  const manifest = [...files].map(([path, bytes]) => ({ path, sha256: sha256(bytes), size: bytes.length }))
    .sort((a, b) => compareSourcePaths(a.path, b.path));
  const record = { schemaVersion: 1, policy: { ...buildPolicy }, manifest };
  return { files, record, policy: { ...buildPolicy }, descriptor: { directory: "build-input", fileCount: files.size,
    totalBytes, controlDigest: recordDigest(record) } };
}
export async function stageBuildInput(store, job, captured) {
  const control = await store.files(job.id);
  for (const [name, bytes] of captured.files)
    await writeHarnessBuildBytes(control, `build-input/${name}`, bytes);
  await writeHarnessRecord(control, RECORD, captured.record);
}
export async function readBuildInput(store, job, signal) {
  const harness = job.input?.harness, descriptor = harness?.buildInput;
  check(descriptor?.directory === "build-input" && exactHash(descriptor.controlDigest),
    "BUILD_NOT_AUTHORIZED", "This task has no captured native build authority");
  const control = await store.files(job.id), record = await readHarnessRecord(control, RECORD);
  check(record.schemaVersion === 1 && Array.isArray(record.manifest) && record.manifest.length <= HARNESS_LIMITS.files &&
    recordDigest(record) === descriptor.controlDigest && recordDigest(record.policy) === recordDigest(harness.buildPolicy),
  "BUILD_INPUT_CHANGED", "Captured build authority or input manifest changed");
  const files = new Map();
  let totalBytes = 0, previous;
  for (const entry of record.manifest) {
    throwIfAborted(signal); binaryPath(entry.path);
    check(Object.keys(entry).sort().join() === "path,sha256,size" && exactHash(entry.sha256) &&
      Number.isSafeInteger(entry.size) && entry.size >= 0 && entry.size <= HARNESS_LIMITS.fileBytes &&
      (previous === undefined || compareSourcePaths(previous, entry.path) < 0),
    "BUILD_INPUT_CHANGED", "Captured binary manifest changed");
    const bytes = await readHarnessBuildBytes(control, `build-input/${entry.path}`, entry);
    totalBytes += bytes.length;
    check(totalBytes <= BUILD_LIMITS.binaryBytes, "BUILD_INPUT_CHANGED", "Captured binary inputs exceeded their budget");
    files.set(entry.path, bytes); previous = entry.path;
  }
  check(totalBytes === descriptor.totalBytes && files.size === descriptor.fileCount,
    "BUILD_INPUT_CHANGED", "Captured binary inventory differs from admission");
  return { files, policy: record.policy };
}

export async function captureBuildSource(service, ctx, args) {
  const captured = await service.buildSnapshot(ctx, args);
  const binary = await readBuildInput(service.store, captured.job, ctx.signal);
  const files = new Map(captured.files);
  for (const [name, bytes] of binary.files) {
    check(!files.has(name), "BUILD_INPUT_CONFLICT", "A text proposal cannot replace an immutable binary build input");
    files.set(name, bytes);
  }
  const names = new Set(files.keys());
  for (const name of names) {
    const parts = name.split("/");
    for (let depth = 1; depth < parts.length; depth++)
      check(!names.has(parts.slice(0, depth).join("/")), "BUILD_INPUT_CONFLICT", "Build overlay conflicts with a directory path");
  }
  const manifest = [...files].map(([path, bytes]) => ({ path, sha256: sha256(bytes), size: bytes.length }))
    .sort((a, b) => compareSourcePaths(a.path, b.path));
  return { ...captured, files, manifest, fingerprint: recordDigest(manifest), policy: binary.policy };
}
