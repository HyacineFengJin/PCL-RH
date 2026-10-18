import { useEffect, useRef, useState } from "react";
import {
  FileText,
  LoaderCircle,
  RotateCcw,
  ScanLine,
  TriangleAlert,
  X,
} from "lucide-react";
import { serviceError, t } from "./i18n";
import type { Api } from "./types";
import "./launch-failure.css";
import { FailureTools } from "./FailureTools";

export type FailureRunStatus = {
  stage: string;
  message: string;
  version: string | null;
  pid: number | null;
  exit_code: number | null;
  root_id: string | null;
  root_path: string | null;
};
export type LaunchFailureReport = {
  version: string | null;
  exit_code: number | null;
  message: string;
  findings: { code: string; evidence: string[] }[];
  log_name: string | null;
  log_tail: string | null;
  log_error: string | null;
  log_truncated: boolean;
  log_scope: "attempt" | "history" | "none";
  content_revision: string;
};
type Scope = {
  expected: FailureRunStatus;
  key: string;
  api: Api;
  native: boolean;
  disabled: boolean;
};
type Result = {
  scope: Scope;
  status: "loading" | "ready" | "error";
  report?: LaunchFailureReport;
  error?: string;
};
type Props = {
  expected: FailureRunStatus;
  scopeKey: string;
  api: Api;
  native: boolean;
  disabled: boolean;
  canInspect: boolean;
  onInspect: () => void;
  onLogs: (name: string | null) => void;
  onClose: () => void;
};
const findingCodes = [
  "java_runtime",
  "java_version",
  "java_memory",
  "java_option",
  "missing_dependency",
  "graphics",
  "graphics_handoff",
  "mod_dependency",
  "mod_incompatible",
  "mod_duplicate",
  "mixin",
  "mod_loading",
  "unknown",
] as const;
type FindingCode = (typeof findingCodes)[number];
function isFindingCode(code: string): code is FindingCode {
  return (findingCodes as readonly string[]).includes(code);
}
function sameRun(left: FailureRunStatus, right: FailureRunStatus) {
  return (
    left.stage === right.stage &&
    left.message === right.message &&
    left.version === right.version &&
    left.pid === right.pid &&
    left.exit_code === right.exit_code &&
    left.root_id === right.root_id &&
    left.root_path === right.root_path
  );
}

/** A report and its navigation actions belong to one captured failed run.
 * Replace ownership during render so old replies and saved handlers are retired
 * before effects run, including when a busy flag changes and later clears. */
export function LaunchFailure({
  expected,
  scopeKey,
  api,
  native,
  disabled,
  canInspect,
  onInspect,
  onLogs,
  onClose,
}: Props) {
  const owner = useRef<Scope>({
    expected: { ...expected },
    key: scopeKey,
    api,
    native,
    disabled,
  });
  if (
    !sameRun(owner.current.expected, expected) ||
    owner.current.key !== scopeKey ||
    owner.current.api !== api ||
    owner.current.native !== native ||
    owner.current.disabled !== disabled
  )
    owner.current = {
      expected: { ...expected },
      key: scopeKey,
      api,
      native,
      disabled,
    };
  const scope = owner.current;
  const live = useRef(true);
  const retired = useRef<Scope | null>(null);
  const generation = useRef(0);
  const operation = useRef<{ scope: Scope; generation: number } | null>(null);
  const callbacks = useRef({ canInspect, onInspect, onLogs, onClose });
  callbacks.current = { canInspect, onInspect, onLogs, onClose };
  const [result, setResult] = useState<Result | null>(null);
  const latestResult = useRef(result);
  latestResult.current = result;
  const closeButton = useRef<HTMLButtonElement>(null);
  const publish = (next: Result) => {
    // Saved log actions stop owning their report as soon as a new reply lands.
    latestResult.current = next;
    setResult(next);
  };
  const current = () =>
    live.current && owner.current === scope && retired.current !== scope;
  const allowed = () => current() && scope.native && !scope.disabled;
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
  async function refresh() {
    if (!allowed() || operation.current?.scope === scope) return;
    const request = ++generation.current;
    operation.current = { scope, generation: request };
    publish({ scope, status: "loading" });
    try {
      const report = await scope.api<LaunchFailureReport>(
        "launch_failure_report",
        {
          expected: { ...scope.expected },
        },
      );
      if (!owns(request)) return;
      if (
        report.version !== scope.expected.version ||
        report.exit_code !== scope.expected.exit_code
      )
        throw new Error(t("failure.wrongRun"));
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
    const previousFocus = document.activeElement as HTMLElement | null;
    closeButton.current?.focus();
    return () => {
      live.current = false;
      generation.current++;
      operation.current = null;
      if (previousFocus?.isConnected) previousFocus.focus();
    };
  }, []);
  useEffect(() => {
    void refresh();
    return () => {
      if (operation.current?.scope === scope) {
        generation.current++;
        operation.current = null;
      }
    };
  }, [scope]);

  const displayed = result?.scope === scope ? result : null;
  const readable = native && !disabled;
  const loading = readable && (!displayed || displayed.status === "loading");
  const report = readable ? displayed?.report : undefined;
  const findings = report?.findings.length
    ? report.findings
    : [{ code: "unknown", evidence: [] }];
  const inspect = () => {
    if (!allowed() || !callbacks.current.canInspect) return;
    const navigate = callbacks.current.onInspect;
    retire();
    callbacks.current.onClose();
    navigate();
  };
  const logs = () => {
    if (
      !allowed() ||
      operation.current ||
      latestResult.current !== displayed ||
      !report?.log_name
    )
      return;
    const navigate = callbacks.current.onLogs;
    retire();
    callbacks.current.onClose();
    navigate(report.log_name);
  };

  return (
    <div
      className="modal-shade ce-failure-shade"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) close();
      }}
    >
      <section
        className="ce-failure-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="launch-failure-title"
        aria-busy={loading}
        tabIndex={-1}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            close();
            return;
          }
          if (event.key !== "Tab") return;
          event.stopPropagation();
          const controls = [
            ...event.currentTarget.querySelectorAll<HTMLElement>(
              "button:not(:disabled),summary,pre[tabindex='0']",
            ),
          ];
          const first = controls[0],
            last = controls[controls.length - 1];
          const outside = !controls.includes(
            document.activeElement as HTMLElement,
          );
          if (!first) {
            event.preventDefault();
            event.currentTarget.focus();
          } else if (
            event.shiftKey &&
            (document.activeElement === first || outside)
          ) {
            event.preventDefault();
            last?.focus();
          } else if (
            !event.shiftKey &&
            (document.activeElement === last || outside)
          ) {
            event.preventDefault();
            first.focus();
          }
        }}
      >
        <header className="ce-failure-heading">
          <TriangleAlert size={22} aria-hidden="true" />
          <h2 id="launch-failure-title">{t("failure.open")}</h2>
          <button
            className="icon-button"
            type="button"
            aria-label={t("failure.close")}
            onClick={close}
          >
            <X size={18} aria-hidden="true" />
          </button>
        </header>
        <div className="ce-failure-body">
          <div className="ce-failure-summary">
            <p>{report?.message || scope.expected.message}</p>
            {(scope.expected.version !== null ||
              scope.expected.exit_code !== null) && (
              <dl>
                {scope.expected.version !== null && (
                  <>
                    <dt>{t("failure.version")}</dt>
                    <dd>{scope.expected.version}</dd>
                  </>
                )}
                {scope.expected.exit_code !== null && (
                  <>
                    <dt>{t("failure.exitCode")}</dt>
                    <dd>{scope.expected.exit_code}</dd>
                  </>
                )}
              </dl>
            )}
          </div>
          {!readable ? (
            <p className="ce-failure-state" role="status">
              {t(native ? "failure.disabled" : "failure.desktopOnly")}
            </p>
          ) : loading ? (
            <p className="ce-failure-state" role="status">
              <LoaderCircle className="spin" size={18} aria-hidden="true" />
              {t("failure.loading")}
            </p>
          ) : displayed?.status === "error" ? (
            <div className="ce-failure-error" role="alert">
              <strong>{t("failure.failed")}</strong>
              <p>{displayed.error}</p>
            </div>
          ) : report ? (
            <>
              <p className="ce-failure-help">{t("failure.help")}</p>
              <div className="ce-failure-findings">
                {findings.map((finding, index) => {
                  const code = isFindingCode(finding.code)
                    ? finding.code
                    : "unknown";
                  return (
                    <section
                      className="ce-failure-finding"
                      key={`${finding.code}-${index}`}
                    >
                      <h3>{t(`failure.${code}.title`)}</h3>
                      <p>{t(`failure.${code}.suggestion`)}</p>
                      {finding.evidence.length > 0 && (
                        <details className="ce-failure-evidence">
                          <summary>{t("failure.evidence")}</summary>
                          <ul>
                            {finding.evidence.map((line, lineIndex) => (
                              <li key={lineIndex}>
                                <code>{line}</code>
                              </li>
                            ))}
                          </ul>
                        </details>
                      )}
                    </section>
                  );
                })}
              </div>
              {report.content_revision && (
                <FailureTools
                  key={report.content_revision}
                  api={api}
                  expected={expected}
                  revision={report.content_revision}
                  scopeKey={scopeKey}
                  disabled={!readable || loading}
                />
              )}
              {report.log_error && (
                <div
                  className="ce-failure-error ce-failure-log-error"
                  role="status"
                >
                  <strong>{t("failure.logReadFailed")}</strong>
                  <p>{report.log_error}</p>
                </div>
              )}
              {report.log_tail ? (
                <details className="ce-failure-log">
                  <summary>{t("failure.logTail")}</summary>
                  {report.log_name && (
                    <p className="ce-failure-log-name">{report.log_name}</p>
                  )}
                  {report.log_truncated && (
                    <p className="ce-failure-help">
                      {t("failure.logTruncated")}
                    </p>
                  )}
                  {report.log_scope === "history" && (
                    <p className="ce-failure-help">{t("failure.historyLog")}</p>
                  )}
                  <pre>{report.log_tail}</pre>
                </details>
              ) : !report.log_error ? (
                <p className="ce-failure-help ce-failure-no-log">
                  {t(report.log_name ? "failure.emptyLog" : "failure.noLog")}
                </p>
              ) : null}
            </>
          ) : null}
        </div>
        <footer className="ce-failure-footer">
          <button
            className="ce-button primary"
            type="button"
            disabled={!readable || !canInspect}
            onClick={inspect}
          >
            <ScanLine size={16} aria-hidden="true" />
            {t("failure.inspect")}
          </button>
          {report?.log_name && (
            <button
              className="ce-button"
              type="button"
              disabled={!readable || loading}
              onClick={logs}
            >
              <FileText size={16} aria-hidden="true" />
              {t("failure.viewLogs")}
            </button>
          )}
          <button
            className="ce-button"
            type="button"
            disabled={!readable || loading}
            onClick={() => void refresh()}
          >
            <RotateCcw size={16} aria-hidden="true" />
            {t("failure.refresh")}
          </button>
          <button
            ref={closeButton}
            className="ce-button"
            type="button"
            onClick={close}
          >
            {t("failure.close")}
          </button>
        </footer>
      </section>
    </div>
  );
}
