/** Trusted bundled tools have one manifest owner. Native admission captures an
 * exact grant list; later plugin additions cannot widen a persisted task. This
 * is registration/lifetime management, not a third-party execution sandbox. */
import { readFileSync } from "node:fs";
import { check } from "../experimental/runtime/src/errors.mjs";

const bundled = JSON.parse(readFileSync(new URL("./plugins.json", import.meta.url), "utf8"));
const MODES = ["ask", "plan", "execute"];
const LEGACY_TOOLS = new Set([
  "harness.list_files", "harness.read_file", "harness.search_files",
  "harness.report_plan", "harness.propose_changes",
]);
const toolName = (value) => typeof value === "string" && value.length <= 80 &&
  /^[a-z][a-z0-9_]*\.[a-z][a-z0-9_]*$/.test(value);
const equalModes = (left, right) => Array.isArray(left) &&
  left.length === right.length && left.every((value, index) => value === right[index]);

function declarations(manifests) {
  check(Array.isArray(manifests) && manifests.length <= 128, "INVALID_TOOL_MANIFEST", "Expected bundled plugin manifests");
  const owners = new Map(), tools = new Map();
  for (const manifest of manifests) {
    check(manifest && typeof manifest.id === "string" && !owners.has(manifest.id) &&
      typeof manifest.version === "string" && manifest.version.length <= 80 &&
      Array.isArray(manifest.dependencies) && Array.isArray(manifest.capabilities),
    "INVALID_TOOL_MANIFEST", "Invalid or duplicate bundled tool owner");
    const owned = [];
    check(Array.isArray(manifest.tools ?? []) && (manifest.tools ?? []).length <= 64,
      "INVALID_TOOL_MANIFEST", "Invalid bundled tool declarations");
    for (const tool of manifest.tools ?? []) {
      check(tool && Object.keys(tool).sort().join() === "capability,modes,name" &&
        toolName(tool.name) && !tools.has(tool.name) &&
        typeof tool.capability === "string" && manifest.capabilities.includes(tool.capability) &&
        Array.isArray(tool.modes) && tool.modes.length > 0 &&
        new Set(tool.modes).size === tool.modes.length && tool.modes.every(mode => MODES.includes(mode)),
      "INVALID_TOOL_MANIFEST", "Tools require a unique name, declared capability and modes");
      const grant = Object.freeze({ name: tool.name, owner: manifest.id,
        capability: tool.capability, modes: Object.freeze([...tool.modes]), version: manifest.version });
      tools.set(grant.name, grant); owned.push(grant);
    }
    owners.set(manifest.id, Object.freeze({ id: manifest.id, version: manifest.version,
      dependencies: Object.freeze([...manifest.dependencies]), tools: Object.freeze(owned) }));
  }
  for (const owner of owners.values())
    check(owner.dependencies.every(id => owners.has(id)), "INVALID_TOOL_MANIFEST", "Unknown bundled dependency");
  return { owners, tools };
}

function enabledOwners(ids, catalog) {
  const enabled = ids === undefined ? [...catalog.owners.keys()] : ids;
  check(Array.isArray(enabled) && enabled.length <= 128 && new Set(enabled).size === enabled.length &&
    enabled.every(id => typeof id === "string" && catalog.owners.has(id)),
  "INVALID_TOOL_SCOPE", "Native plugin scope contains unknown or duplicate owners");
  const active = new Set(enabled);
  for (const id of active)
    check(catalog.owners.get(id).dependencies.every(dependency => active.has(dependency)),
      "INVALID_TOOL_SCOPE", "Native plugin scope is missing a dependency");
  return [...active].sort();
}

export function captureHarnessToolScope(ids, mode, manifests = bundled) {
  check(MODES.includes(mode), "INVALID_TOOL_SCOPE", "Choose a captured Harness mode");
  const catalog = declarations(manifests), enabledPlugins = enabledOwners(ids, catalog);
  const toolGrants = enabledPlugins.flatMap(id => catalog.owners.get(id).tools)
    .filter(tool => tool.modes.includes(mode)).sort((a, b) => a.name.localeCompare(b.name))
    .map(tool => ({ ...tool, modes: [...tool.modes] }));
  return { enabledPlugins, toolGrants };
}

export class HarnessToolCatalog {
  #catalog;
  #registry;
  #registrations = new Map();
  #generations = new Map();
  #releaseScope;
  #disposed = false;
  constructor(registry, manifests = bundled) {
    this.#catalog = declarations(manifests);
    this.#registry = registry;
    this.#releaseScope = registry.setScopeResolver("harness", job => this.resolve(job));
  }
  register(tool) {
    check(!this.#disposed, "TOOL_UNAVAILABLE", "Bundled tool catalog retired");
    const grant = this.#catalog.tools.get(tool.name);
    check(grant && grant.owner === tool.owner && grant.capability === tool.capability &&
      equalModes(tool.modes, grant.modes) &&
      (!grant.modes.includes("ask") || tool.replaySafety === "read-only"),
    "TOOL_OWNERSHIP_DENIED", "Tool implementation differs from its manifest owner or scope");
    const release = this.#registry.contribute(tool);
    this.#registrations.set(tool.name, { owner: tool.owner, release });
    return this;
  }
  forPlugin(owner) {
    check(this.#catalog.owners.has(owner) && !this.#disposed, "TOOL_OWNERSHIP_DENIED", "Unknown bundled tool owner");
    const generation = this.#generations.get(owner) ?? 0;
    const registrar = { register: tool => {
      check(!this.#disposed && (this.#generations.get(owner) ?? 0) === generation,
        "TOOL_UNAVAILABLE", "This plugin registration lifetime retired");
      check(tool.owner === owner, "TOOL_OWNERSHIP_DENIED", "A plugin can register only its own tools");
      this.register(tool); return registrar;
    } };
    return registrar;
  }
  retire(owner) {
    // Pending callbacks from the old plugin cannot recreate its contributions.
    // A fresh forPlugin registrar starts the next lifetime explicitly.
    this.#generations.set(owner, (this.#generations.get(owner) ?? 0) + 1);
    for (const [name, registration] of this.#registrations)
      if (registration.owner === owner) {
        registration.release(); this.#registrations.delete(name);
      }
  }
  resolve(job) {
    check(!this.#disposed && job.profile === "harness" && MODES.includes(job.input?.harness?.mode),
      "INVALID_TOOL_SCOPE", "Invalid captured Harness tool scope");
    const harness = job.input.harness, mode = harness.mode;
    if (harness.toolGrants === undefined) {
      // Older tasks predate tool grants. Do not grant newer modules or history
      // tools just because the installed host now has more contributions.
      return [...this.#catalog.tools.values()].filter(tool => LEGACY_TOOLS.has(tool.name) &&
        tool.modes.includes(mode) && this.#registrations.has(tool.name)).map(tool => tool.name);
    }
    check(Array.isArray(harness.enabledPlugins), "INVALID_TOOL_SCOPE", "Captured tool grants require an exact owner scope");
    const enabled = new Set(enabledOwners(harness.enabledPlugins, this.#catalog));
    check(Array.isArray(harness.toolGrants) && harness.toolGrants.length <= 128 &&
      new Set(harness.toolGrants.map(grant => grant?.name)).size === harness.toolGrants.length,
    "INVALID_TOOL_SCOPE", "Invalid captured tool grant list");
    const allowed = [];
    for (const captured of harness.toolGrants) {
      const current = this.#catalog.tools.get(captured?.name);
      check(current && Object.keys(captured).sort().join() === "capability,modes,name,owner,version" &&
        enabled.has(captured.owner) && current.owner === captured.owner && current.version === captured.version &&
        current.capability === captured.capability && equalModes(captured.modes, current.modes) &&
        captured.modes.includes(mode), "TOOL_SCOPE_CHANGED", "Captured tool ownership or version changed");
      if (this.#registrations.has(current.name)) allowed.push(current.name);
    }
    return allowed;
  }
  view() {
    return [...this.#catalog.owners.values()].filter(owner => owner.tools.length).map(owner => ({
      id: owner.id, version: owner.version,
      tools: owner.tools.map(tool => ({ ...tool, modes: [...tool.modes], registered: this.#registrations.has(tool.name) })),
    }));
  }
  dispose() {
    if (this.#disposed) return;
    this.#disposed = true;
    for (const registration of this.#registrations.values()) registration.release();
    this.#registrations.clear(); this.#releaseScope();
  }
}
