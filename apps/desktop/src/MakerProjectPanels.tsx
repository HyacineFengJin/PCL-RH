/** Project charter drafts belong to the session; source versions belong to the
 * host. Selecting an old base creates no destructive rollback or game effect. */
import { useEffect, useState } from "react";
import { GitBranch } from "lucide-react";
import type { Api } from "./types";
import type {
  MakerProject,
  ProjectCheckpoint,
} from "./experimentalProjectTypes";
import type { MakerDraft } from "./makerWorkspaceState";
import { experimentalCall as call } from "./experimentalTypes";
import { useMakerScope } from "./useMakerScope";
import { ExperimentalProjectCompare } from "./ExperimentalProjectCompare";
import { t, formatDate } from "./i18n";
export function MakerCharter({
  api,
  native,
  project,
  draft,
  onSaved,
}: {
  api: Api;
  native: boolean;
  project: MakerProject;
  draft: MakerDraft;
  onSaved: () => Promise<void>;
}) {
  const scope = useMakerScope(api, native, project.id),
    [notes, setNotes] = useState(
      draft.notes.get(project.id) ?? {
        name: project.name,
        notes: project.notes,
        revision: project.revision,
      },
    );
  useEffect(() => {
    setNotes(
      draft.notes.get(project.id) ?? {
        name: project.name,
        notes: project.notes,
        revision: project.revision,
      },
    );
  }, [project.id, project.revision]);
  function edit(patch: Partial<typeof notes>) {
    scope.change(() => {
      const value = { ...notes, ...patch };
      setNotes(value);
      draft.notes.set(project.id, value);
    });
  }
  return (
    <div className="ma-charter">
      <p>{t("agent.charterHelp")}</p>
      <label>
        {t("experimental.projectName")}
        <input
          className="ce-field"
          maxLength={120}
          value={notes.name}
          onChange={(e) => edit({ name: e.target.value })}
        />
      </label>
      <label>
        {t("agent.charter")}
        <textarea
          className="ce-field"
          rows={16}
          maxLength={8192}
          value={notes.notes}
          onChange={(e) => edit({ notes: e.target.value })}
        />
      </label>
      {notes.revision !== project.revision && (
        <p className="ma-risk">{t("agent.staleCharter")}</p>
      )}
      <button
        className="ce-button"
        disabled={
          scope.busy ||
          !notes.name.trim() ||
          notes.revision !== project.revision
        }
        onClick={() =>
          void scope.perform(async () => {
            await call(api, "project_update", {
              id: project.id,
              expectedRevision: notes.revision,
              name: notes.name,
              notes: notes.notes,
              archived: project.archived,
            });
            if (!scope.responseCurrent()) return;
            draft.notes.delete(project.id);
            await onSaved();
          })
        }
      >
        {t("agent.save")}
      </button>
      {notes.revision !== project.revision && (
        <button
          className="ce-button"
          disabled={scope.busy}
          onClick={() => edit({ revision: project.revision })}
        >
          {t("agent.adoptRevision")}
        </button>
      )}
      <button
        className="ma-text-button ma-risk"
        disabled={scope.busy}
        onClick={() =>
          void scope.perform(async () => {
            await call(api, "project_update", {
              id: project.id,
              expectedRevision: project.revision,
              name: project.name,
              notes: project.notes,
              archived: !project.archived,
            });
            if (scope.responseCurrent()) await onSaved();
          })
        }
      >
        {project.archived
          ? t("agent.unarchiveProject")
          : t("agent.archiveProject")}
      </button>
      {scope.error && (
        <p className="experimental-error" role="alert">
          {scope.error}
        </p>
      )}
    </div>
  );
}
export function MakerVersions({
  api,
  native,
  project,
  point,
  onBase,
  onBranch,
  onSource,
}: {
  api: Api;
  native: boolean;
  project: MakerProject;
  point: ProjectCheckpoint;
  onBase: (id: string) => void;
  onBranch: (id: string) => void;
  onSource: (jobId: string, operationId: string) => void;
}) {
  return (
    <div className="ma-versions">
      <p>{t("agent.versionsHelp")}</p>
      {[...project.checkpoints].reverse().map((version) => (
        <section
          className={`ma-version-node ${point?.id === version.id ? "selected" : ""}`}
          key={version.id}
        >
          <GitBranch size={16} />
          <div>
            <strong>
              v{project.checkpoints.indexOf(version) + 1} · {version.label}
            </strong>
            <small>{formatDate(version.createdAt)}</small>
            <span>
              {t("agent.compileNotRun")} · {t("agent.gameNotRun")}
            </span>
            <div>
              <button className="ce-button" onClick={() => onBase(version.id)}>
                {t("agent.useVersion")}
              </button>
              <button
                className="ce-button"
                disabled={project.archived || version.workflow !== "maker"}
                onClick={() => onBranch(version.id)}
              >
                {t("agent.branchThread")}
              </button>
              <button
                className="ma-text-button"
                onClick={() => onSource(version.jobId, version.operationId)}
              >
                {t("agent.openSource")}
              </button>
            </div>
          </div>
        </section>
      ))}
      <ExperimentalProjectCompare
        api={api}
        native={native}
        project={project}
        checkpointId={point.id}
      />
    </div>
  );
}
