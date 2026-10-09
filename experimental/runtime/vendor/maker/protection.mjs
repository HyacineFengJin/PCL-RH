/** Only receipt-verified ownership metadata informs the graph and captured AI
 * write restrictions. This does not grant edits or replace Python's full audit. */
import { createHash } from "node:crypto";
import { readIndexFile } from "./source-index.mjs";
const file = "creator-ownership.json";
function parse(raw, inventory) {
  const meta = JSON.parse(raw);
  if (
    meta.schema_version !== 1 ||
    !meta.files ||
    Array.isArray(meta.files) ||
    typeof meta.files !== "object"
  )
    throw new Error("Unsupported source ownership");
  const entries = new Map(
    inventory
      .filter((entry) => entry.path !== file)
      .map((entry) => [entry.path, entry]),
  );
  if (Object.keys(meta.files).length !== entries.size)
    throw new Error("Ownership inventory mismatch");
  const protectedPaths = [];
  for (const [name, record] of Object.entries(meta.files)) {
    const entry = entries.get(name);
    if (
      !entry ||
      !record ||
      !["template", "user", "locked"].includes(record.owner) ||
      record.sha256 !== entry.sha256
    )
      throw new Error("Ownership record differs from source receipt");
    if (record.owner === "locked") protectedPaths.push(name);
  }
  return protectedPaths.sort();
}
export function snapshotProtection(snapshot) {
  if (!snapshot.files[file]) return [];
  const inventory = Object.entries(snapshot.files).map(([path, bytes]) => ({
    path,
    sha256: createHash("sha256").update(bytes).digest("hex"),
  }));
  return parse(snapshot.files[file].toString("utf8"), inventory);
}
export async function indexedProtection(source) {
  const entry = source.snapshot.inventory.find((value) => value.path === file);
  if (!entry)
    return { fingerprint: source.snapshot.fingerprint, protectedPaths: [] };
  if (entry.bytes > 256000)
    throw new Error("Ownership metadata exceeds 256 KB");
  let raw = "",
    offset = 0;
  do {
    const chunk = await readIndexFile(source, { path: file, offset });
    raw += chunk.content;
    offset = chunk.nextOffset;
  } while (offset !== null);
  return {
    fingerprint: source.snapshot.fingerprint,
    protectedPaths: parse(raw, source.snapshot.inventory),
  };
}
