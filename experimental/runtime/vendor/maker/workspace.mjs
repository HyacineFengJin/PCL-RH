/** Project organization is separate from immutable source checkpoints. Units
 * describe features and related paths, never grants or compiled game objects.
 * One host owns serialization; independent CAS revisions retain unsaved edits
 * when another editor changes organization. A rejected write leaves both the
 * previous organization and every source/protection record intact.
 */
import { randomUUID } from "node:crypto";
import path from "node:path";
import { Workspace } from "../../src/workspace.mjs";
import { recordDigest } from "../../src/artifact-snapshot.mjs";
export const UNIT_KINDS = Object.freeze([
  "items",
  "blocks",
  "entities",
  "structures",
  "rules",
  "systems",
  "assets",
  "other",
]);
export const WORKSPACE_LIMITS = Object.freeze({
  units: 4096,
  bytes: 1_000_000,
  page: 50,
  notes: 6000,
  links: 32,
});
const UUID = /^[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12}(?![\s\S])/;
function need(ok, message) {
  if (!ok) throw new Error(message);
}
function text(value, max, multiline = false) {
  need(
    typeof value === "string" &&
      value.length <= max &&
      !/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f\p{Cf}\p{Cs}]/u.test(
        value,
      ),
    "Invalid workspace text",
  );
  need(
    multiline || (value.trim().length && !/[\t\r\n]/.test(value)),
    "Invalid workspace name",
  );
  return multiline ? value : value.trim();
}
function exact(value, keys) {
  need(
    value &&
      typeof value === "object" &&
      !Array.isArray(value) &&
      Object.keys(value).sort().join() === [...keys].sort().join(),
    "Invalid workspace fields",
  );
}
function unit(value) {
  exact(value, ["id", "name", "kind", "state", "notes", "files", "updatedAt"]);
  need(
    UUID.test(value.id) &&
      UNIT_KINDS.includes(value.kind) &&
      ["planned", "in_progress", "ready"].includes(value.state),
    "Invalid feature identity or state",
  );
  text(value.name, 120);
  text(value.notes, WORKSPACE_LIMITS.notes, true);
  need(
    Array.isArray(value.files) &&
      value.files.length <= WORKSPACE_LIMITS.links &&
      new Set(value.files).size === value.files.length,
    "Invalid related file list",
  );
  for (const file of value.files) {
    text(file, 512);
    need(
      !file.startsWith("/") &&
        !file.includes("\\") &&
        file.split("/").every((part) => part && part !== "." && part !== ".."),
      "Use project-relative related file paths",
    );
  }
  need(
    typeof value.updatedAt === "string" &&
      new Date(value.updatedAt).toISOString() === value.updatedAt,
    "Invalid feature timestamp",
  );
  return value;
}
export class MakerWorkspace {
  constructor(root, projects) {
    this.files = new Workspace(path.join(root, "maker-workspaces"));
    this.projects = projects;
  }
  async init() {
    await this.files.init();
    return this;
  }
  async load({ id, expectedRevision }) {
    const project = await this.projects.get(id);
    need(
      project.revision === expectedRevision,
      "Project changed; refresh before editing its workspace",
    );
    let record;
    try {
      record = JSON.parse((await this.files.read(`${id}.json`)).content);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
      record = { schemaVersion: 1, projectId: id, units: [] };
    }
    exact(record, ["schemaVersion", "projectId", "units"]);
    need(
      record.schemaVersion === 1 &&
        record.projectId === id &&
        Array.isArray(record.units) &&
        record.units.length <= WORKSPACE_LIMITS.units,
      "Unsupported project workspace",
    );
    const ids = new Set();
    for (const value of record.units) {
      unit(value);
      need(!ids.has(value.id), "Duplicate feature ID");
      ids.add(value.id);
    }
    need(
      Buffer.byteLength(JSON.stringify(record)) <= WORKSPACE_LIMITS.bytes,
      "Workspace exceeds 1 MB",
    );
    return { project, record, workspaceRevision: recordDigest(record) };
  }
  async list(args) {
    const { offset = 0, kind = "all", query = "" } = args;
    need(
      Number.isSafeInteger(offset) &&
        offset >= 0 &&
        offset <= WORKSPACE_LIMITS.units,
      "Invalid feature cursor",
    );
    need(
      kind === "all" || UNIT_KINDS.includes(kind),
      "Invalid feature category",
    );
    text(query, 120, true);
    const { project, record, workspaceRevision } = await this.load(args);
    const counts = Object.fromEntries(
      UNIT_KINDS.map((key) => [
        key,
        record.units.filter((value) => value.kind === key).length,
      ]),
    );
    const filtered = record.units.filter(
      (value) =>
        (kind === "all" || value.kind === kind) &&
        value.name.toLowerCase().includes(query.toLowerCase()),
    );
    const units = filtered
      .slice(offset, offset + WORKSPACE_LIMITS.page)
      .map(({ id, name, kind, state, files, updatedAt }) => ({
        id,
        name,
        kind,
        state,
        fileCount: files.length,
        updatedAt,
      }));
    return {
      id: project.id,
      revision: project.revision,
      workspaceRevision,
      counts,
      total: record.units.length,
      filteredCount: filtered.length,
      offset,
      nextOffset:
        offset + units.length < filtered.length ? offset + units.length : null,
      units,
    };
  }
  async read(args) {
    const value = await this.load(args),
      selected = value.record.units.find((unit) => unit.id === args.unitId);
    need(selected, "Select an existing project feature");
    return {
      id: value.project.id,
      revision: value.project.revision,
      workspaceRevision: value.workspaceRevision,
      unit: selected,
    };
  }
  async save(args) {
    const { project, record, workspaceRevision } = await this.load(args);
    need(!project.archived, "Unarchive the project before editing features");
    need(
      workspaceRevision === args.expectedWorkspaceRevision,
      "Workspace changed; refresh before saving. Your local draft is retained.",
    );
    exact(args.unit, ["id", "name", "kind", "state", "notes", "files"]);
    const value = unit({
      ...args.unit,
      id: args.unit.id || randomUUID(),
      name: text(args.unit.name, 120),
      updatedAt: new Date().toISOString(),
    });
    const index = record.units.findIndex((old) => old.id === value.id);
    need(!args.unit.id || index >= 0, "Select an existing project feature");
    if (index < 0) record.units.push(value);
    else record.units[index] = value;
    need(
      record.units.length <= WORKSPACE_LIMITS.units,
      "Project feature limit is 4096",
    );
    const serialized = JSON.stringify(record);
    need(
      Buffer.byteLength(serialized) <= WORKSPACE_LIMITS.bytes,
      "Workspace exceeds 1 MB",
    );
    await this.files.write(`${project.id}.json`, serialized);
    return {
      id: project.id,
      revision: project.revision,
      workspaceRevision: recordDigest(record),
      unit: value,
    };
  }
}
