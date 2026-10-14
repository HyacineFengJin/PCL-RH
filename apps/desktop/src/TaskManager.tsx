import { t, formatNumber, type MessageKey } from "./i18n";
import { useEffect, useRef, useState } from "react";
import {
  Check,
  Download,
  LoaderCircle,
  MoreHorizontal,
  TriangleAlert,
  X,
} from "lucide-react";
import type { DownloadStatus, DownloadStep } from "./DownloadPanel";
import type { Api } from "./types";
import {
  taskDisplayName,
  taskHasFloatingEntry,
  taskIsActive,
  taskAggregate,
} from "./taskLifecycle";
import "./task-manager.css";
function taskStepLabel(kind: DownloadStatus["kind"], step: DownloadStep) {
  if (kind === "instance_repair") {
    switch (step.id) {
      case "repair-check":
        return t("inspection.repairPlan");
      case "repair-download":
        return t("inspection.repairFiles");
      case "repair-verify":
        return t("inspection.repairCheck");
    }
  }
  if (kind === "java_install") {
    switch (step.id) {
      case "java-metadata":
        return t("java.stepMetadata");
      case "java-files":
        return t("java.stepFiles");
      case "java-probe":
        return t("java.stepProbe");
      case "java-publish":
        return t("java.stepPublish");
    }
  }
  return step.label;
}
function repairFallbackSteps(status: DownloadStatus): DownloadStep[] {
  const phases = ["repair-check", "repair-download", "repair-verify"];
  const current = Math.max(0, phases.indexOf(status.phase || "repair-check"));
  return phases.map((id, index) => {
    const step: DownloadStep = {
      id,
      label: "",
      state:
        status.stage === "complete" || index < current
          ? "complete"
          : index === current && status.stage !== "queued"
            ? "running"
            : "pending",
    };
    return { ...step, label: taskStepLabel("instance_repair", step) };
  });
}
function javaFallbackSteps(status: DownloadStatus): DownloadStep[] {
  const phases = ["java-metadata", "java-files", "java-probe", "java-publish"];
  const current = Math.max(0, phases.indexOf(status.phase || "java-metadata"));
  return phases.map((id, index) => {
    const step: DownloadStep = {
      id,
      label: "",
      state:
        status.stage === "complete" || index < current
          ? "complete"
          : index === current && status.stage !== "queued"
            ? "running"
            : "pending",
    };
    return { ...step, label: taskStepLabel("java_install", step) };
  });
}
const taskActionMessages: Partial<
  Record<NonNullable<DownloadStatus["kind"]>, MessageKey>
> = {
  resource_operation: "task.resourceOperation",
  java_install: "java.download",
  toolbox_download: "task.toolboxDownload",
  resource_save: "task.resourceSave",
  launcher_logs: "task.launcherLogs",
  instance_reset: "ui.reset",
  instance_repair: "inspection.repair",
  instance_export: "nav.export",
  instance_rename: "ui.rename",
  instance_import: "ui.import",
  instance_delete: "ui.delete",
  instance_restore: "ui.restore",
  resource_update_restore: "task.modRestore",
  resource_update: "nav.modUpdates",
  resource_download: "task.resourceInstall",
};
export function instanceTaskAction(kind: DownloadStatus["kind"]) {
  return t((kind && taskActionMessages[kind]) || "ui.install");
}
export function useDownloadSpeed(status: DownloadStatus) {
  const sample = useRef<{
    bytes: number;
    time: number;
    task: string | null;
  } | null>(null);
  const [speed, setSpeed] = useState<number | null>(null);
  useEffect(() => {
    const now = performance.now();
    const old = sample.current;
    const bytes = status.network_bytes;
    const running = [
      "queued",
      "downloading",
      "preparing",
      "processing",
    ].includes(status.stage);
    const task =
      status.task_id || `${status.root_id || ""}:${status.version || ""}`;
    if (bytes === undefined || !running) {
      setSpeed(null);
      sample.current = null;
    } else if (!old || old.task !== task || bytes < old.bytes) {
      setSpeed(null);
      sample.current = { bytes, time: now, task };
    } else if (now - old.time >= 250) {
      setSpeed(((bytes - old.bytes) * 1000) / (now - old.time));
      sample.current = { bytes, time: now, task };
    }
  }, [status]);
  return speed;
}
export function TaskStatistics({
  status,
  speed,
  tasks,
}: {
  status: DownloadStatus;
  speed: number | null;
  tasks?: readonly DownloadStatus[];
}) {
  const terminalCancelled =
    status.stage === "cancelled" || status.stage === "idle";
  const progress = Number.isFinite(status.progress)
    ? status.progress!
    : status.total > 0
      ? status.completed / status.total
      : null;
  const percent = terminalCancelled
    ? null
    : status.stage === "complete"
      ? 100
      : progress === null
        ? null
        : Math.max(0, Math.min(99.9, progress * 100));
  return (
    <div className="ce-task-statistics">
      <div>
        <span
          title={
            tasks && tasks.length > 1 ? t("task.aggregateHint") : undefined
          }
        >
          {t("task.progress")}
        </span>
        <strong>
          {percent === null
            ? "—"
            : `${formatNumber(percent, { minimumFractionDigits: 1, maximumFractionDigits: 1 })}%`}
        </strong>
      </div>
      <div>
        <span>{t("task.speed")}</span>
        <strong>
          {terminalCancelled ||
          speed === null ||
          status.kind === "instance_export" ||
          status.kind === "instance_rename" ||
          status.kind === "instance_import" ||
          status.kind === "instance_delete" ||
          status.kind === "instance_restore" ||
          status.kind === "resource_update_restore" ||
          status.kind === "launcher_logs" ||
          status.kind === "resource_operation"
            ? "—"
            : `${formatNumber(speed / 1048576, { minimumFractionDigits: 2, maximumFractionDigits: 2 })} MiB/s`}
        </strong>
      </div>
      <div>
        <span>{t("task.remaining")}</span>
        <strong>
          {tasks
            ? !terminalCancelled &&
              tasks.filter(taskIsActive).every((task) => task.total > 0)
              ? formatNumber(
                  tasks
                    .filter(taskIsActive)
                    .reduce(
                      (sum, task) =>
                        sum + Math.max(0, task.total - task.completed),
                      0,
                    ),
                )
              : "—"
            : !terminalCancelled && status.total > 0
              ? formatNumber(Math.max(0, status.total - status.completed))
              : "—"}
        </strong>
      </div>
    </div>
  );
}
function TaskCard({
  api,
  status,
  native,
  onNotify,
  onStatusChange,
  onCancelled,
  onDismiss,
}: {
  api: Api;
  status: DownloadStatus;
  native: boolean;
  onNotify: (s: string) => void;
  onStatusChange?: (status: DownloadStatus) => void;
  onCancelled?: (status: DownloadStatus) => void;
  onDismiss?: (status: DownloadStatus) => void;
}) {
  const [cancelling, setCancelling] = useState(false);
  const cancelPending = useRef(false);
  const mounted = useRef(true);
  const latest = useRef({
    api,
    status,
    native,
    onNotify,
    onStatusChange,
    onCancelled,
    onDismiss,
  });
  latest.current = {
    api,
    status,
    native,
    onNotify,
    onStatusChange,
    onCancelled,
    onDismiss,
  };
  const notifiedCancellation = useRef<string | null>(null);
  const requestedCancellation = useRef<string | null>(null);
  const taskKey =
    status.task_id ||
    `${status.root_id || status.root_path || ""}:${status.version || ""}`;
  const currentTask = useRef(taskKey);
  currentTask.current = taskKey;
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  useEffect(() => {
    cancelPending.current = false;
    requestedCancellation.current = null;
    setCancelling(false);
  }, [taskKey, api]);
  function finishCancellation(next: DownloadStatus) {
    const key =
      next.task_id ||
      `${next.root_id || next.root_path || ""}:${next.version || ""}`;
    if (
      next.stage !== "cancelled" ||
      requestedCancellation.current !== key ||
      key !== currentTask.current ||
      notifiedCancellation.current === key
    )
      return;
    notifiedCancellation.current = key;
    latest.current.onCancelled?.(next);
  }
  useEffect(() => {
    if (status.stage === "cancelled") finishCancellation(status);
  }, [status]);
  function dismiss() {
    if (
      !mounted.current ||
      currentTask.current !== taskKey ||
      latest.current.api !== api ||
      latest.current.status.stage !== "error"
    )
      return;
    latest.current.onDismiss?.(latest.current.status);
  }
  const active = ["queued", "downloading", "preparing", "processing"].includes(
    status.stage,
  );
  const action = instanceTaskAction(status.kind);
  const cancelAction =
    status.kind === "resource_download" ? t("task.resourceDownload") : action;
  const phase =
    status.phase ||
    (status.stage === "downloading" ? "downloading" : "metadata");
  const current =
    status.kind === "resource_update_restore"
      ? phase === "resource-update-restore-apply"
        ? 1
        : 0
      : status.kind === "instance_import"
        ? Math.max(
            0,
            [
              "import-check",
              "import-extract",
              "import-commit",
              "import-cleanup",
            ].indexOf(phase),
          )
        : status.kind === "instance_delete" ||
            status.kind === "instance_restore"
          ? phase === "committing"
            ? 1
            : 0
          : status.kind === "resource_save" ||
              status.kind === "resource_download" ||
              status.kind === "resource_update"
            ? (status.kind === "resource_save" && phase === "committing") ||
              phase === "save-publish" ||
              phase === "resource-publish" ||
              phase === "resource-update-publish" ||
              phase === "installing"
              ? 2
              : status.stage === "downloading"
                ? 1
                : 0
            : phase === "metadata"
              ? 0
              : phase === "downloading"
                ? 1
                : phase === "installing"
                  ? 2
                  : phase === "complete"
                    ? 2
                    : 0;
  const fallbackLabels =
    status.kind === "resource_operation"
      ? [t("task.resourceFileWork")]
      : status.kind === "resource_save"
        ? [
            t("task.resourceMetadata"),
            t("task.downloadVerify"),
            t("task.savePublish"),
          ]
        : status.kind === "launcher_logs" || status.kind === "toolbox_download"
          ? []
          : status.kind === "instance_import"
            ? [
                t("task.importCheck"),
                t("task.importExtract"),
                t("task.importPublish"),
                t("task.importCleanup"),
              ]
            : status.kind === "instance_delete" ||
                status.kind === "instance_restore"
              ? [
                  t("task.instanceCheck"),
                  t("task.instanceMove"),
                  t("task.instanceRecord"),
                ]
              : status.kind === "resource_update_restore"
                ? [t("task.updateUndoCheck"), t("task.updateUndoApply")]
                : status.kind === "resource_update"
                  ? [
                      t("task.updateCheck"),
                      t("task.downloadVerify"),
                      t("task.updateApply"),
                    ]
                  : status.kind === "resource_download"
                    ? [
                        t("task.resourceMetadata"),
                        t("task.downloadVerify"),
                        t("task.resourceApply"),
                      ]
                    : [
                        t("task.minecraftMetadata"),
                        t("task.minecraftDownload"),
                        t("task.minecraftInstall"),
                      ];
  const steps: DownloadStep[] = status.steps?.length
    ? status.steps
    : status.kind === "instance_repair"
      ? repairFallbackSteps(status)
      : status.kind === "java_install"
        ? javaFallbackSteps(status)
        : fallbackLabels.map((label, index) => ({
            id: `legacy-${index}`,
            label,
            state:
              status.stage === "complete"
                ? "complete"
                : active && status.stage !== "queued" && index === current
                  ? "running"
                  : "pending",
          }));
  async function cancel() {
    if (
      !native ||
      !mounted.current ||
      !latest.current.native ||
      !active ||
      !["queued", "downloading", "preparing", "processing"].includes(
        latest.current.status.stage,
      ) ||
      currentTask.current !== taskKey ||
      latest.current.api !== api ||
      status.can_cancel === false ||
      latest.current.status.can_cancel === false ||
      cancelPending.current
    )
      return;
    const target = taskKey;
    const targetApi = api;
    cancelPending.current = true;
    requestedCancellation.current = target;
    setCancelling(true);
    try {
      const next = await api<DownloadStatus>("download_cancel", {
        taskId: status.task_id ?? null,
      });
      if (
        !mounted.current ||
        currentTask.current !== target ||
        latest.current.api !== targetApi
      )
        return;
      const returnedTask =
        next.task_id ||
        `${next.root_id || next.root_path || ""}:${next.version || ""}`;
      if (returnedTask !== target) return;
      if (
        ["complete", "error"].includes(latest.current.status.stage) &&
        next.stage !== latest.current.status.stage
      )
        return;
      latest.current.onStatusChange?.(next);
      finishCancellation(next);
    } catch (e) {
      if (
        mounted.current &&
        currentTask.current === target &&
        latest.current.api === targetApi
      )
        latest.current.onNotify(String(e));
    } finally {
      if (
        mounted.current &&
        currentTask.current === target &&
        latest.current.api === targetApi
      ) {
        cancelPending.current = false;
        setCancelling(false);
      }
    }
  }
  const repairResult =
    status.kind === "instance_repair" &&
    status.result &&
    "unsupported" in status.result &&
    Array.isArray(status.result.unsupported)
      ? status.result
      : null;
  const unsupported = repairResult?.unsupported.slice(0, 32) || [];
  const unsupportedOmitted = Math.max(
    0,
    (repairResult?.unsupported_total || 0) - unsupported.length,
  );
  if (["idle", "cancelled", "complete"].includes(status.stage)) return null;
  return (
    <section className={`ce-card ce-task-card ${status.stage}`}>
      <div className="ce-task-heading">
        <strong
          title={
            status.root_path &&
            !["resource_save", "toolbox_download"].includes(status.kind || "")
              ? t("task.root", { path: status.root_path })
              : undefined
          }
        >
          {taskDisplayName(status)} {action}
        </strong>
        <button
          className="icon-button"
          aria-label={
            status.stage === "error" && onDismiss
              ? t("task.dismiss")
              : cancelling
                ? t("task.cleanupPending")
                : t("task.cancel", { action: cancelAction })
          }
          title={
            status.stage === "error" && onDismiss
              ? t("task.dismiss")
              : cancelling
                ? t("task.cleanup")
                : t("task.cancel", { action: cancelAction })
          }
          disabled={
            status.stage === "error" && onDismiss
              ? false
              : !native || !active || status.can_cancel === false || cancelling
          }
          onClick={() =>
            status.stage === "error" && onDismiss ? dismiss() : void cancel()
          }
        >
          {cancelling ? (
            <LoaderCircle size={17} className="spin" />
          ) : (
            <X size={17} />
          )}
        </button>
      </div>
      {status.stage === "queued" && (
        <p className="ce-task-message" role="status">
          {t("task.queued")}
        </p>
      )}
      <div className="ce-task-steps">
        {steps.map((step) => (
          <div key={step.id} className={`ce-task-step ${step.state}`}>
            <span className="ce-task-step-status">
              {step.state === "complete" ? (
                <Check size={22} strokeWidth={3} />
              ) : step.state === "running" && status.stage === "error" ? (
                <TriangleAlert size={18} />
              ) : step.state === "running" && Number.isFinite(step.progress) ? (
                `${Math.round(Math.max(0, Math.min(1, step.progress!)) * 100)}%`
              ) : (
                <MoreHorizontal size={24} />
              )}
            </span>
            <span>{taskStepLabel(status.kind, step)}</span>
          </div>
        ))}
      </div>
      <p
        className={`ce-task-message ${status.stage === "error" ? "auth-error" : "muted"}`}
        role="status"
      >
        {status.stage === "error" && <TriangleAlert size={16} />}{" "}
        {cancelling ? t("task.cancelling") : status.message}
      </p>
      {status.error && status.error !== status.message && (
        <p className="ce-task-message auth-error" role="alert">
          {t("task.originalWarning", { message: status.error })}
        </p>
      )}
      {!!repairResult?.unsupported_total && (
        <details className="ce-task-repair-details">
          <summary>
            {t("inspection.repairUnsupported", {
              count: formatNumber(repairResult.unsupported_total),
            })}
          </summary>
          <ul>
            {unsupported.map((item, index) => (
              <li key={`${index}:${item.path}`}>
                <span>{item.path}</span>
                <small>{item.reason}</small>
              </li>
            ))}
          </ul>
          {unsupportedOmitted > 0 && (
            <p>
              {t("inspection.moreFiles", {
                count: formatNumber(unsupportedOmitted),
              })}
            </p>
          )}
        </details>
      )}
    </section>
  );
}

/** Existing CE cards each retain their own cancellation request and ID. Removing
 * another completed card cannot replace this card's handler or cleanup lifetime. */
export function TaskManager(props: {
  api: Api;
  status?: DownloadStatus;
  tasks?: readonly DownloadStatus[];
  native: boolean;
  onNotify: (s: string) => void;
  onStatusChange?: (status: DownloadStatus) => void;
  onCancelled?: (status: DownloadStatus) => void;
  onDismiss?: (status: DownloadStatus) => void;
}) {
  const records = props.tasks || (props.status ? [props.status] : []);
  const visible = records.filter(taskHasFloatingEntry);
  return (
    <div className="ce-task-list">
      {records.map((task) => (
        <TaskCard
          key={task.task_id || `${task.root_id}:${task.version}`}
          {...props}
          status={task}
        />
      ))}
      {!visible.length && (
        <section className="ce-card ce-task-empty">
          <Download size={36} />
          <p>{t("task.empty")}</p>
        </section>
      )}
    </div>
  );
}

/** Network counters are sampled per job before summing rates. Adding or removing
 * jobs cannot turn their already-transferred bytes into an artificial speed. */
export function useAggregateDownloadSpeed(tasks: readonly DownloadStatus[]) {
  const samples = useRef(new Map<string, { bytes: number; time: number }>());
  const [speed, setSpeed] = useState<number | null>(null);
  useEffect(() => {
    const now = performance.now(),
      next = new Map<string, { bytes: number; time: number }>();
    let rate = 0,
      measured = false;
    for (const task of tasks) {
      if (
        !taskIsActive(task) ||
        [
          "resource_operation",
          "instance_export",
          "instance_rename",
          "instance_import",
          "instance_delete",
          "instance_restore",
          "resource_update_restore",
          "launcher_logs",
        ].includes(task.kind || "") ||
        task.stage === "queued" ||
        task.network_bytes === undefined ||
        !task.task_id
      )
        continue;
      const old = samples.current.get(task.task_id),
        bytes = task.network_bytes;
      if (old && bytes >= old.bytes && now - old.time >= 250) {
        rate += ((bytes - old.bytes) * 1000) / (now - old.time);
        measured = true;
        next.set(task.task_id, { bytes, time: now });
      } else
        next.set(
          task.task_id,
          old && bytes >= old.bytes ? old : { bytes, time: now },
        );
    }
    samples.current = next;
    setSpeed(measured ? rate : null);
  }, [tasks]);
  return speed;
}
export { taskAggregate };
