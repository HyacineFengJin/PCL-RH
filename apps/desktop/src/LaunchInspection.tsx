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

type Scope = { id: string; key: string; api: Api };
type Result = {
  scope: Scope;
  status: "loading" | "ready" | "error";
  report?: LaunchInspectionReport;
  error?: string;
};
type Props = {
  id: string;
  scopeKey: string;
  api: Api;
  native: boolean;
  disabled: boolean;
  onClose: () => void;
  onManageJava: (major: number) => void;
};

/** This read owns no launch/task state. Render-time scope replacement retires
 * old replies and saved handlers before effect cleanup, including instance
 * changes within the same root. Closing retires immediately before navigation. */
export function LaunchInspection({
  id,
  scopeKey,
  api,
  native,
  disabled,
  onClose,
  onManageJava,
}: Props) {
  const owner = useRef<Scope>({ id, key: scopeKey, api });
  if (
    owner.current.id !== id ||
    owner.current.key !== scopeKey ||
    owner.current.api !== api
  )
    owner.current = { id, key: scopeKey, api };
  const scope = owner.current;
  const live = useRef(true);
  const retired = useRef<Scope | null>(null);
  const generation = useRef(0);
  const operation = useRef<{ scope: Scope; generation: number } | null>(null);
  const admission = useRef({ native, disabled });
  admission.current = { native, disabled };
  const callbacks = useRef({ onClose, onManageJava });
  callbacks.current = { onClose, onManageJava };
  const [result, setResult] = useState<Result | null>(null);
  const latestResult = useRef(result);
  latestResult.current = result;
  const closeButton = useRef<HTMLButtonElement>(null);
  const publish = (next: Result) => {
    // Retire saved result actions when the reply lands, before React renders it.
    latestResult.current = next;
    setResult(next);
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
    if (!allowed() || operation.current?.scope === scope) return;
    const request = ++generation.current;
    operation.current = { scope, generation: request };
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
    void inspect();
    return () => {
      if (operation.current?.scope === scope) {
        generation.current++;
        operation.current = null;
      }
    };
  }, [scope, native, disabled]);

  const displayed = result?.scope === scope ? result : null;
  const readable = native && !disabled;
  const loading = readable && (!displayed || displayed.status === "loading");
  const report = readable ? displayed?.report : undefined;
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
        aria-busy={loading}
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
          {!readable ? (
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
        </div>
        <footer className="ce-inspection-footer">
          <button
            className="ce-button primary"
            type="button"
            disabled={!readable || loading}
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
