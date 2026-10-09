import { useEffect, useState } from "react";
import { CeSelect } from "./CeSelect";
import { InstanceOperationDialog } from "./instanceOperationUi";
import type { Api } from "./types";
import type { MakerProject } from "./experimentalProjectTypes";
import {
  experimentalCall as call,
  type JobSummary,
  type JobView,
} from "./experimentalTypes";
import { useMakerScope } from "./useMakerScope";
import { t, serviceError } from "./i18n";
/** Existing Maker and Porter receipts can enter the same project library. */
export function MakerRegisterProject({
  api,
  native,
  jobs,
  onClose,
  onRecorded,
}: {
  api: Api;
  native: boolean;
  jobs: JobSummary[];
  onClose: () => void;
  onRecorded: (project: MakerProject) => Promise<void>;
}) {
  const [jobId, setJobId] = useState(""),
    [job, setJob] = useState<JobView | null>(null),
    [op, setOp] = useState(""),
    [name, setName] = useState(""),
    [notes, setNotes] = useState(""),
    [label, setLabel] = useState("");
  const scope = useMakerScope(api, native, jobId);
  useEffect(() => {
    setJob(null);
    setOp("");
    const current = scope.readTicket();
    if (native && jobId)
      void call<JobView>(api, "job_read", { jobId })
        .then((value) => {
          if (!current()) return;
          if (value.summary.jobId !== jobId)
            throw new Error(t("maker.responseMismatch"));
          setJob(value);
        })
        .catch((e) => {
          if (current()) scope.setError(serviceError(e));
        });
  }, [scope.scope]);
  return (
    <InstanceOperationDialog
      title={t("maker.registerExisting")}
      titleId="ma-register-title"
      busy={scope.busy}
      committing={scope.busy}
      confirmLabel={t("maker.registerProject")}
      confirmDisabled={
        !native || scope.busy || !op || !name.trim() || !label.trim()
      }
      onClose={onClose}
      onConfirm={() =>
        void scope.perform(async () => {
          const value = await call<MakerProject>(api, "project_create", {
            jobId,
            operationId: op,
            name,
            notes,
            label,
          });
          if (scope.responseCurrent()) await onRecorded(value);
        })
      }
    >
      <p>{t("maker.registrationIntro")}</p>
      <label>
        {t("maker.sourceTask")}
        <CeSelect value={jobId} onChange={(e) => setJobId(e.target.value)}>
          <option value="">{t("maker.selectTask")}</option>
          {jobs
            .filter((job) => !["queued", "running"].includes(job.status))
            .map((job) => (
              <option key={job.jobId} value={job.jobId}>
                {job.workflow} · {job.jobId}
              </option>
            ))}
        </CeSelect>
      </label>
      <label>
        {t("maker.sourceOutput")}
        <CeSelect value={op} onChange={(e) => setOp(e.target.value)}>
          <option value="">{t("maker.selectSource")}</option>
          {job?.artifacts.map((artifact) => (
            <option key={artifact.operationId} value={artifact.operationId}>
              {artifact.operationId}
            </option>
          ))}
        </CeSelect>
      </label>
      <label>
        {t("experimental.projectName")}
        <input
          className="ce-field"
          value={name}
          maxLength={120}
          onChange={(e) => setName(e.target.value)}
        />
      </label>
      <label>
        {t("agent.charter")}
        <textarea
          className="ce-field"
          value={notes}
          maxLength={8192}
          onChange={(e) => setNotes(e.target.value)}
        />
      </label>
      <label>
        {t("experimental.versionLabel")}
        <input
          className="ce-field"
          value={label}
          maxLength={120}
          onChange={(e) => setLabel(e.target.value)}
        />
      </label>
      {scope.error && (
        <p className="experimental-error" role="alert">
          {scope.error}
        </p>
      )}
    </InstanceOperationDialog>
  );
}
