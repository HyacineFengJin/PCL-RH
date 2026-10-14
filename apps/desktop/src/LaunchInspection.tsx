import { useEffect, useRef, useState } from "react";
import { Check, LoaderCircle, RotateCcw, ScanLine, X } from "lucide-react";
import { formatNumber, serviceError, t } from "./i18n";
import type { Api } from "./types";
import "./launch-inspection.css";

export type LaunchInspectionReport = {
  id: string;
  minecraft_version: string;
  loader: string;
  game_dir: string;
  /** Zero means metadata did not establish a Java requirement. */
  java_major: number;
  java_exact: boolean;
  java_path: string | null;
  java_error: string | null;
  missing_files: string[];
  missing_total: number;
  issues: string[];
  ready: boolean;
};

type Scope = { id: string; key: string; api: Api; revision?: string };
type Result = {
  scope: Scope;
  status: "loading" | "ready" | "error";
  report?: LaunchInspectionReport;
  error?: string;
};
type RepairResult = {
  scope: Scope;
  status: "pending" | "error";
  error?: string;
};
type RepairRequest = {
  scope: Scope;
  onTaskStart: (id: string, reveal: boolean) => void;
};
type Props = {
  id: string;
  scopeKey: string;
  api: Api;
  native: boolean;
  disabled: boolean;
  revision?: string;
  onClose: () => void;
  onManageJava: (major: number) => void;
  onTaskStart: (id: string, reveal: boolean) => void;
};

/** Render-time scope replacement retires reports and saved handlers. A repair
 * submission survives close/unmount because an admitted task belongs to the
 * global task manager; late success registers it without changing navigation. */
export function LaunchInspection({
  id,
  scopeKey,
  api,
  native,
  disabled,
  revision,
  onClose,
  onManageJava,
  onTaskStart,
}: Props) {
  const owner = useRef<Scope>({ id, key: scopeKey, api, revision });
  if (
    owner.current.id !== id ||
    owner.current.key !== scopeKey ||
    owner.current.api !== api ||
    owner.current.revision !== revision
  )
    owner.current = { id, key: scopeKey, api, revision };
  const scope = owner.current;
  const live = useRef(true);
  const retired = useRef<Scope | null>(null);
  const generation = useRef(0);
  const operation = useRef<{ scope: Scope; generation: number } | null>(null);
  const repairOperation = useRef<RepairRequest | null>(null);
  const admission = useRef({ native, disabled });
  admission.current = { native, disabled };
  const callbacks = useRef({ onClose, onManageJava, onTaskStart });
  callbacks.current = { onClose, onManageJava, onTaskStart };
  const [result, setResult] = useState<Result | null>(null);
  const latestResult = useRef(result);
  latestResult.current = result;
  const [repairResult, setRepairResult] = useState<RepairResult | null>(null);
  const latestRepair = useRef(repairResult);
  latestRepair.current = repairResult;
  const closeButton = useRef<HTMLButtonElement>(null);
  const publish = (next: Result) => {
    // Retire saved result actions when the reply lands, before React renders it.
    latestResult.current = next;
    setResult(next);
  };
  const publishRepair = (next: RepairResult | null) => {
    latestRepair.current = next;
    setRepairResult(next);
  };

  const current = () =>
    live.current && owner.current === scope && retired.current !== scope;
  const allowed = () =>
    current() && admission.current.native && !admission.current.disabled;
  const owns = (request: number) =>
    allowed() &&
    operation.current?.scope === scope &&
    operation.current.generation === request;
  const retire = () => {
    retired.current = scope;
    generation.current++;
    operation.current = null;
  };
  const close = () => {
    if (!current()) return;
    retire();
    callbacks.current.onClose();
  };
  async function inspect() {
    if (
      !allowed() ||
      operation.current?.scope === scope ||
      repairOperation.current?.scope === scope
    )
      return;
    const request = ++generation.current;
    operation.current = { scope, generation: request };
    // Keep another scope's pending marker so its completion still schedules a
    // render when the global submission guard is released.
    if (latestRepair.current?.status !== "pending") publishRepair(null);
    publish({ scope, status: "loading" });
    try {
      const report = await scope.api<LaunchInspectionReport>(
        "inspect_instance",
        { id: scope.id },
      );
      if (!owns(request)) return;
      if (report.id !== scope.id)
        throw new Error(t("inspection.wrongInstance"));
      publish({ scope, status: "ready", report });
    } catch (error) {
      if (owns(request))
        publish({ scope, status: "error", error: serviceError(error) });
    } finally {
      if (
        operation.current?.scope === scope &&
        operation.current.generation === request
      )
        operation.current = null;
    }
  }
  useEffect(() => {
    live.current = true;
    closeButton.current?.focus();
    return () => {
      live.current = false;
      generation.current++;
      operation.current = null;
    };
  }, []);
  useEffect(() => {
    // Native busy flags can change after repair admission. Keep its report and
    // retry error until an explicit recheck or a genuinely new scope.
    if (latestRepair.current?.scope !== scope) void inspect();
    return () => {
      if (operation.current?.scope === scope) {
        generation.current++;
        operation.current = null;
      }
    };
  }, [scope, native, disabled]);

  const displayed = result?.scope === scope ? result : null;
  const displayedRepair = repairResult?.scope === scope ? repairResult : null;
  const repairing = displayedRepair?.status === "pending";
  const readable = native && !disabled;
  const loading =
    readable && !repairing && (!displayed || displayed.status === "loading");
  const report = readable || repairing ? displayed?.report : undefined;
  const javaProblem = report && (!report.java_path || !!report.java_error);
  const missingPaths = report?.missing_files.slice(0, 32) ?? [];
  const omitted = Math.max(
    0,
    (report?.missing_total ?? 0) - missingPaths.length,
  );
  const manageJava = () => {
    if (
      !allowed() ||
      operation.current ||
      repairOperation.current ||
      latestResult.current !== displayed ||
      !javaProblem ||
      !report ||
      report.java_major <= 0
    )
      return;
    retire();
    callbacks.current.onClose();
    callbacks.current.onManageJava(report.java_major);
  };
  const repairable =
    !!report && report.missing_total > 0 && report.issues.length === 0;
  const canRepair =
    allowed() &&
    repairable &&
    !!scope.revision?.trim() &&
    !operation.current &&
    !repairOperation.current;
  async function repair() {
    if (
      !allowed() ||
      !repairable ||
      !scope.revision?.trim() ||
      operation.current ||
      repairOperation.current ||
      latestResult.current !== displayed
    )
      return;
    const request = { scope, onTaskStart: callbacks.current.onTaskStart };
    repairOperation.current = request;
    publishRepair({ scope, status: "pending" });
    let task: { id: string };
    try {
      task = await scope.api<{ id: string }>("instance_repair_start", {
        id: scope.id,
        revision: scope.revision,
      });
    } catch (error) {
      if (current())
        publishRepair({ scope, status: "error", error: serviceError(error) });
      else if (live.current) publishRepair(null);
      return;
    } finally {
      if (repairOperation.current === request) repairOperation.current = null;
    }
    if (current()) {
      retire();
      try {
        callbacks.current.onClose();
      } finally {
        request.onTaskStart(task.id, true);
      }
    } else {
      if (live.current) publishRepair(null);
      request.onTaskStart(task.id, false);
    }
  }

  return (
    <div
      className="modal-shade ce-inspection-shade"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) close();
      }}
    >
      <section
        className="ce-inspection-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="launch-inspection-title"
        aria-busy={loading || repairing}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            close();
          }
          if (event.key !== "Tab") return;
          event.stopPropagation();
          const controls = [
            ...event.currentTarget.querySelectorAll<HTMLElement>(
              "button:not(:disabled),summary",
            ),
          ];
          const first = controls[0],
            last = controls[controls.length - 1];
          if (!first) event.preventDefault();
          else if (event.shiftKey && document.activeElement === first) {
            event.preventDefault();
            last?.focus();
          } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault();
            first.focus();
          }
        }}
      >
        <header className="ce-inspection-heading">
          <ScanLine size={22} aria-hidden="true" />
          <h2 id="launch-inspection-title">{t("inspection.open")}</h2>
          <button
            className="icon-button"
            type="button"
            aria-label={t("inspection.close")}
            onClick={close}
          >
            <X size={18} aria-hidden="true" />
          </button>
        </header>
        <div className="ce-inspection-body">
          <p className="ce-inspection-help">{t("inspection.help")}</p>
          {!readable && !repairing ? (
            <p className="ce-inspection-state" role="status">
              {t(native ? "inspection.disabled" : "inspection.desktopOnly")}
            </p>
          ) : loading ? (
            <p className="ce-inspection-state" role="status">
              <LoaderCircle className="spin" size={19} aria-hidden="true" />
              {t("inspection.loading")}
            </p>
          ) : displayed?.status === "error" ? (
            <div className="ce-inspection-error" role="alert">
              <strong>{t("inspection.failed")}</strong>
              <p>{displayed.error}</p>
            </div>
          ) : report ? (
            <>
              <p
                className={`ce-inspection-state ${report.ready ? "is-ready" : "is-problem"}`}
                role="status"
              >
                {report.ready && <Check size={19} aria-hidden="true" />}
                {t(
                  report.ready
                    ? "inspection.ready"
                    : "inspection.needsAttention",
                )}
              </p>
              <section className="ce-inspection-section">
                <h3>{t("instance.overview")}</h3>
                <dl>
                  <dt>{t("instance.name")}</dt>
                  <dd>{report.id}</dd>
                  <dt>{t("ui.version")}</dt>
                  <dd>{report.minecraft_version || "—"}</dd>
                  <dt>{t("ui.loader")}</dt>
                  <dd>{report.loader || "—"}</dd>
                  <dt>{t("inspection.gameDirectory")}</dt>
                  <dd>{report.game_dir || "—"}</dd>
                </dl>
              </section>
              <section className="ce-inspection-section">
                <h3>{t("inspection.java")}</h3>
                <p className={javaProblem ? "ce-inspection-problem" : ""}>
                  {t(
                    report.java_major <= 0
                      ? "inspection.javaUnknown"
                      : javaProblem
                        ? "inspection.javaUnavailable"
                        : "inspection.javaFound",
                  )}
                </p>
                {report.java_major > 0 && (
                  <p>
                    {t(
                      report.java_exact
                        ? "java.requiresExact"
                        : "java.requiresNewer",
                      {
                        version: report.java_major,
                      },
                    )}
                  </p>
                )}
                {report.java_path && (
                  <p className="ce-inspection-path">{report.java_path}</p>
                )}
                {report.java_error && (
                  <p className="ce-inspection-problem">{report.java_error}</p>
                )}
                {javaProblem && report.java_major > 0 && (
                  <div className="ce-inspection-java-action">
                    <button
                      className="ce-button"
                      type="button"
                      disabled={!readable || !!repairOperation.current}
                      onClick={manageJava}
                    >
                      {t("inspection.manageJava")}
                    </button>
                    <small>{t("inspection.javaSettingsHelp")}</small>
                  </div>
                )}
              </section>
              <section className="ce-inspection-section">
                <h3>{t("inspection.files")}</h3>
                <p
                  className={
                    report.missing_total > 0 ? "ce-inspection-problem" : ""
                  }
                >
                  {report.missing_total > 0
                    ? t("inspection.filesMissing", {
                        count: formatNumber(report.missing_total),
                      })
                    : t(
                        report.issues.length > 0
                          ? "inspection.filesIncomplete"
                          : "inspection.filesPresent",
                      )}
                </p>
                {missingPaths.length > 0 && (
                  <details className="ce-inspection-files">
                    <summary>{t("inspection.missingPaths")}</summary>
                    <ul>
                      {missingPaths.map((path, index) => (
                        <li key={`${index}:${path}`}>{path}</li>
                      ))}
                    </ul>
                    {omitted > 0 && (
                      <p>
                        {t("inspection.moreFiles", {
                          count: formatNumber(omitted),
                        })}
                      </p>
                    )}
                  </details>
                )}
                {repairable && (
                  <div className="ce-inspection-repair-action">
                    <button
                      className="ce-button primary"
                      type="button"
                      disabled={!canRepair}
                      onClick={() => void repair()}
                    >
                      {repairing && (
                        <LoaderCircle
                          className="spin"
                          size={16}
                          aria-hidden="true"
                        />
                      )}
                      {t(
                        repairing
                          ? "inspection.repairStarting"
                          : "inspection.repair",
                      )}
                    </button>
                    <p className="ce-inspection-repair-help">
                      {t("inspection.repairHelp")}
                    </p>
                  </div>
                )}
              </section>
              {report.issues.length > 0 && (
                <section className="ce-inspection-section">
                  <h3>{t("inspection.otherIssues")}</h3>
                  <ul className="ce-inspection-problem">
                    {report.issues.map((issue, index) => (
                      <li key={`${index}:${issue}`}>{issue}</li>
                    ))}
                  </ul>
                </section>
              )}
            </>
          ) : null}
          {displayedRepair?.status === "error" && (
            <div className="ce-inspection-error" role="alert">
              <strong>{t("inspection.repairFailed")}</strong>
              <p>{displayedRepair.error}</p>
            </div>
          )}
        </div>
        <footer className="ce-inspection-footer">
          <button
            className="ce-button primary"
            type="button"
            disabled={!readable || loading || !!repairOperation.current}
            onClick={() => void inspect()}
          >
            <RotateCcw size={16} aria-hidden="true" />
            {t("inspection.recheck")}
          </button>
          <button
            ref={closeButton}
            className="ce-button"
            type="button"
            onClick={close}
          >
            {t("inspection.close")}
          </button>
        </footer>
      </section>
    </div>
  );
}
