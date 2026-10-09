/** Session drafts are separate from host-owned threads, dossiers and immutable
 * sources. A content unit describes scope, not a template or compilation proof. */
export const makerKinds = [
  "items",
  "blocks",
  "entities",
  "structures",
  "rules",
  "systems",
  "assets",
  "other",
] as const;
export type MakerKind = (typeof makerKinds)[number];
export type MakerUnit = {
  id: string;
  name: string;
  kind: MakerKind;
  state: "planned" | "in_progress" | "ready";
  notes: string;
  files: string[];
  updatedAt?: string;
};
export type MakerUnitPage = {
  id: string;
  revision: string;
  workspaceRevision: string;
  counts: Record<MakerKind, number>;
  total: number;
  filteredCount: number;
  offset: number;
  nextOffset: number | null;
  units: (Omit<MakerUnit, "notes" | "files"> & { fileCount: number })[];
};
export type MakerUnitResult = {
  id: string;
  revision: string;
  workspaceRevision: string;
  unit: MakerUnit;
};
export type MakerRoute = {
  projectId: string;
  section: "units" | "unit";
  kind?: MakerKind;
  unitId?: string;
};
export function createProjectManagerDraft() {
  return {
    agentProject: "",
    agentThread: "",
    agentMode: "execute" as import("./MakerAgentState").MakerMode,
    agentScopes: new Map<
      string,
      import("./MakerAgentState").MakerScopeHint[]
    >(),
    notes: new Map<string, { name: string; notes: string; revision: string }>(),
    units: new Map<string, { unit: MakerUnit; revision: string }>(),
    prompts: new Map<string, string>(),
    selectedVersions: new Map<string, string>(),
    buffers: new Map<
      string,
      { text: string; original: string; sha256: string }
    >(),
    newName: "",
    newGoal: "",
  };
}
export type MakerDraft = ReturnType<typeof createProjectManagerDraft>;
