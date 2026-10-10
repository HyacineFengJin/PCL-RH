/** Native project folders are user-owned, whereas Maker receipts are immutable.
 * Reconcile manual Java/resource edits only in the new job copy. The original
 * folder and its receipt stay untouched; changed source becomes code-locked so
 * a subsequent template regeneration cannot erase handwritten content.
 * Control/build-file changes still need a different adapter or explicit import. */
import { createHash } from "node:crypto";
const metaPath = "creator-ownership.json";
const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const hex = /^[0-9a-f]{64}$/;
const editable = (path) =>
  path !== "src/main/resources/fabric.mod.json" &&
  ((path.startsWith("src/main/java/") && path.endsWith(".java")) ||
    (path.startsWith("src/main/resources/") &&
      /\.(json|mcmeta|txt|lang)$/.test(path)));

export function reconcileNativeOwnership(files) {
  if (!files[metaPath]) return [];
  const meta = JSON.parse(files[metaPath].toString("utf8"));
  if (
    meta.schema_version !== 1 ||
    !meta.files ||
    typeof meta.files !== "object" ||
    Array.isArray(meta.files)
  )
    throw new Error("Unsupported native source ownership");
  const records = meta.files;
  const changed = [];
  for (const [path, record] of Object.entries(records)) {
    if (
      !Object.hasOwn(files, path) ||
      path === metaPath ||
      !record ||
      !["template", "locked", "user"].includes(record.owner) ||
      !hex.test(record.sha256) ||
      (record.baseline_sha256 !== null && !hex.test(record.baseline_sha256))
    )
      throw new Error(
        "Native ownership inventory is invalid; files were retained",
      );
    const sha256 = digest(files[path]);
    if (record.sha256 === sha256) continue;
    if (!editable(path))
      throw new Error(`Modified control file needs an import adapter: ${path}`);
    records[path] = { ...record, owner: "locked", sha256 };
    changed.push(path);
  }
  for (const [path, bytes] of Object.entries(files)) {
    if (path === metaPath || Object.hasOwn(records, path)) continue;
    // Defining an own property avoids treating a filename as an object setter.
    Object.defineProperty(records, path, {
      enumerable: true,
      configurable: true,
      writable: true,
      value: {
        owner: editable(path) ? "locked" : "user",
        sha256: digest(bytes),
        baseline_sha256: null,
      },
    });
    changed.push(path);
  }
  if (changed.length) {
    files[metaPath] = Buffer.from(JSON.stringify(meta, null, 2) + "\n");
    if (
      Object.values(files).reduce((sum, bytes) => sum + bytes.length, 0) >
      256000
    )
      throw new Error("Reconciled source snapshot exceeds 256 KB");
  }
  return changed.sort();
}
