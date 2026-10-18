import { useEffect, useRef, useState } from "react";
import { Download, LoaderCircle, Sparkles, Square } from "lucide-react";
import { serviceError, t } from "./i18n";
import type { Api } from "./types";
import type { FailureRunStatus } from "./LaunchFailure";

export type DiagnosticPreview = {
  report_id: string;
  report_text: string;
  report_truncated: boolean;
  configuration: {
    ready: boolean;
    confirmationToken: string | null;
    warning: string | null;
    selectedPreset: {
      id: string;
      name: string;
      selection: { provider: string; model: string; baseUrl: string };
    } | null;
  };
};
type DiagnosticJob = {
  reportId: string;
  jobId: string;
  status: string;
  output: string;
  error: { message?: string } | string | null;
};
type AiView = {
  phase:
    | "idle"
    | "previewing"
    | "preview"
    | "starting"
    | "running"
    | "done"
    | "error";
  preview?: DiagnosticPreview;
  job?: DiagnosticJob;
  error?: string;
};
const terminal = new Set(["completed", "cancelled", "failed", "interrupted"]);

/** Local analysis is always offline. Only an explicit second click sends the
 * native report bytes to the previewed preset. Jobs own immutable report IDs;
 * leaving this view cancels its job, including a late start acknowledgement. */
export function FailureTools({
  api,
  expected,
  revision,
  scopeKey,
  disabled,
}: {
  api: Api;
  expected: FailureRunStatus;
  revision: string;
  scopeKey: string;
  disabled: boolean;
}) {
  const owner = useRef({
    api,
    revision,
    scopeKey,
    disabled,
    run: JSON.stringify(expected),
  });
  const run = JSON.stringify(expected);
  if (
    owner.current.api !== api ||
    owner.current.revision !== revision ||
    owner.current.scopeKey !== scopeKey ||
    owner.current.disabled !== disabled ||
    owner.current.run !== run
  )
    owner.current = { api, revision, scopeKey, disabled, run };
  const scope = owner.current;
  const live = useRef(true);
  const exporting = useRef(false);
  const aiOperation = useRef<symbol | null>(null);
  const active = useRef<{ api: Api; reportId: string; jobId: string } | null>(
    null,
  );
  const [exportBusy, setExportBusy] = useState(false);
  const [exportResult, setExportResult] = useState<{
    path?: string;
    error?: string;
    warning?: string;
  } | null>(null);
  const [view, setView] = useState<AiView>({ phase: "idle" });
  const latest = useRef(view);
  latest.current = view;
  const current = () =>
    live.current && owner.current === scope && !scope.disabled;
  const reportArgs = () => ({ expected: { ...expected }, revision });
  const cancelJob = (job: { api: Api; reportId: string; jobId: string }) =>
    job
      .api("launch_failure_ai_cancel", {
        reportId: job.reportId,
        jobId: job.jobId,
      })
      .catch(() => {});
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  useEffect(() => {
    setView({ phase: "idle" });
    setExportResult(null);
    return () => {
      aiOperation.current = null;
      const job = active.current;
      active.current = null;
      if (job) void cancelJob(job);
    };
  }, [scope]);
  async function exportReport() {
    if (!current() || exporting.current) return;
    exporting.current = true;
    setExportBusy(true);
    setExportResult(null);
    try {
      const result = await scope.api<{
        status: string;
        path: string | null;
        warning?: string;
      }>("launch_failure_export", reportArgs());
      if (current() && result.status === "complete")
        setExportResult({ path: result.path || "", warning: result.warning });
      if (current() && result.status === "unavailable")
        setExportResult({
          error: result.warning || t("common.desktopUnavailable"),
        });
    } catch (error) {
      if (current()) setExportResult({ error: serviceError(error) });
    } finally {
      exporting.current = false;
      if (live.current) setExportBusy(false);
    }
  }
  async function preview() {
    if (!current() || aiOperation.current || active.current) return;
    const operation = Symbol();
    aiOperation.current = operation;
    setView({ phase: "previewing" });
    try {
      const data = await scope.api<DiagnosticPreview>(
        "launch_failure_ai_preview",
        reportArgs(),
      );
      if (!current() || aiOperation.current !== operation) return;
      if (data.report_id !== scope.revision)
        throw new Error(t("failure.wrongRun"));
      setView({ phase: "preview", preview: data });
    } catch (error) {
      if (current() && aiOperation.current === operation)
        setView({ phase: "error", error: serviceError(error) });
    } finally {
      if (aiOperation.current === operation) aiOperation.current = null;
    }
  }
  async function start() {
    const shown = latest.current;
    const data = shown.preview;
    if (
      !current() ||
      aiOperation.current ||
      shown.phase !== "preview" ||
      !data?.configuration.ready ||
      !data.configuration.confirmationToken ||
      data.report_id !== scope.revision
    )
      return;
    const operation = Symbol();
    aiOperation.current = operation;
    setView({ phase: "starting", preview: data });
    try {
      const result = await scope.api<{ jobId: string; reportId: string }>(
        "launch_failure_ai_start",
        {
          ...reportArgs(),
          confirmationToken: data.configuration.confirmationToken,
        },
      );
      const job = {
        api: scope.api,
        jobId: result.jobId,
        reportId: result.reportId,
      };
      if (!current() || aiOperation.current !== operation) {
        await cancelJob(job);
        return;
      }
      if (result.reportId !== scope.revision) {
        await cancelJob(job);
        throw new Error(t("failure.wrongRun"));
      }
      active.current = job;
      setView({
        phase: "running",
        preview: data,
        job: { ...result, status: "queued", output: "", error: null },
      });
    } catch (error) {
      if (current() && aiOperation.current === operation)
        setView({ phase: "error", preview: data, error: serviceError(error) });
    } finally {
      if (aiOperation.current === operation) aiOperation.current = null;
    }
  }
  const jobId = view.phase === "running" ? view.job?.jobId : undefined;
  useEffect(() => {
    if (!jobId || !current()) return;
    let retired = false,
      working = false;
    async function poll() {
      const job = active.current;
      if (retired || working || !current() || !job || job.jobId !== jobId)
        return;
      working = true;
      try {
        const next = await job.api<DiagnosticJob>("launch_failure_ai_status", {
          reportId: job.reportId,
          jobId: job.jobId,
        });
        if (retired || !current() || active.current !== job) return;
        if (next.jobId !== job.jobId || next.reportId !== job.reportId)
          throw new Error(t("failure.wrongRun"));
        if (terminal.has(next.status)) active.current = null;
        setView((before) => ({
          ...before,
          phase: terminal.has(next.status) ? "done" : "running",
          job: next,
        }));
      } catch (error) {
        if (retired || !current()) return;
        active.current = null;
        void cancelJob(job);
        setView((before) => ({
          ...before,
          phase: "error",
          error: serviceError(error),
        }));
      } finally {
        working = false;
      }
    }
    void poll();
    const timer = window.setInterval(() => void poll(), 700);
    return () => {
      retired = true;
      window.clearInterval(timer);
    };
  }, [scope, jobId]);
  async function stop() {
    const job = active.current;
    if (!current() || !job || aiOperation.current) return;
    const operation = Symbol();
    aiOperation.current = operation;
    setView((before) => ({ ...before }));
    try {
      const result = await job.api<{ cancelled: boolean }>(
        "launch_failure_ai_cancel",
        { reportId: job.reportId, jobId: job.jobId },
      );
      if (!current() || active.current !== job) return;
      if (result.cancelled) {
        active.current = null;
        setView((before) => ({
          ...before,
          phase: "done",
          job: before.job && { ...before.job, status: "cancelled" },
        }));
      }
    } catch (error) {
      if (current())
        setView((before) => ({ ...before, error: serviceError(error) }));
    } finally {
      if (aiOperation.current === operation) aiOperation.current = null;
      if (current()) setView((before) => ({ ...before }));
    }
  }
  const waiting = ["previewing", "starting"].includes(view.phase);
  const selected = view.preview?.configuration.selectedPreset;
  const error =
    view.error ||
    (typeof view.job?.error === "string"
      ? view.job.error
      : view.job?.error?.message);
  return (
    <section className="ce-failure-tools">
      <div className="ce-failure-tool-buttons">
        <button
          type="button"
          className="ce-button"
          disabled={disabled || exportBusy}
          onClick={() => void exportReport()}
        >
          {exportBusy ? (
            <LoaderCircle size={16} className="spin" />
          ) : (
            <Download size={16} />
          )}
          {t("failure.exportReport")}
        </button>
        <button
          type="button"
          className="ce-button"
          disabled={disabled || waiting || view.phase === "running"}
          onClick={() => void preview()}
        >
          {waiting ? (
            <LoaderCircle size={16} className="spin" />
          ) : (
            <Sparkles size={16} />
          )}
          {t("failure.aiAnalyze")}
        </button>
      </div>
      {exportResult?.path !== undefined && (
        <p className="ce-failure-help" role="status" title={exportResult.path}>
          {t("failure.exported")}
        </p>
      )}
      {exportResult?.error && (
        <p className="ce-failure-error" role="alert">
          {exportResult.error}
        </p>
      )}
      {exportResult?.warning && (
        <p className="ce-failure-error" role="status">
          {exportResult.warning}
        </p>
      )}
      {view.phase !== "idle" && (
        <div className="ce-failure-ai">
          {waiting && (
            <p className="ce-failure-state" role="status">
              <LoaderCircle size={16} className="spin" />
              {t(
                view.phase === "previewing"
                  ? "failure.aiPreparing"
                  : "failure.aiStarting",
              )}
            </p>
          )}
          {selected && (
            <p className="ce-failure-ai-target">
              <strong>{selected.name}</strong>
              <span>{selected.selection.model}</span>
              <small>{selected.selection.baseUrl}</small>
            </p>
          )}
          {view.preview && view.phase === "preview" && (
            <>
              <p className="ce-failure-help">{t("failure.aiConsent")}</p>
              <details className="ce-failure-log">
                <summary>{t("failure.aiPayload")}</summary>
                {view.preview.report_truncated && (
                  <p className="ce-failure-help">{t("failure.aiExcerpt")}</p>
                )}
                <pre tabIndex={0}>{view.preview.report_text}</pre>
              </details>
              {view.preview.configuration.warning && (
                <p className="ce-failure-error">
                  {view.preview.configuration.warning}
                </p>
              )}
              {!view.preview.configuration.ready && (
                <p className="ce-failure-help">
                  {t("failure.aiNotConfigured")}
                </p>
              )}
              <button
                type="button"
                className="ce-button primary"
                disabled={
                  disabled ||
                  !view.preview.configuration.ready ||
                  !view.preview.configuration.confirmationToken
                }
                onClick={() => void start()}
              >
                {t("failure.aiSend")}
              </button>
            </>
          )}
          {view.phase === "running" && (
            <div className="ce-failure-tool-buttons">
              <p className="ce-failure-state" role="status">
                <LoaderCircle size={16} className="spin" />
                {t("failure.aiRunning")}
              </p>
              <button
                type="button"
                className="ce-button"
                disabled={disabled || !!aiOperation.current}
                onClick={() => void stop()}
              >
                <Square size={14} />
                {t("failure.aiStop")}
              </button>
            </div>
          )}
          {view.job?.output && (
            <>
              <p className="ce-failure-help">{t("failure.aiReference")}</p>
              <pre className="ce-failure-ai-output" tabIndex={0}>
                {view.job.output}
              </pre>
            </>
          )}
          {view.phase === "done" && (
            <p className="ce-failure-help" role="status">
              {t(
                view.job?.status === "completed"
                  ? "failure.aiCompleted"
                  : view.job?.status === "cancelled"
                    ? "failure.aiCancelled"
                    : "failure.aiFailed",
              )}
            </p>
          )}
          {error && (
            <p className="ce-failure-error" role="alert">
              {error}
            </p>
          )}
        </div>
      )}
    </section>
  );
}
