/** The build plugin displays native receipts and edits future task grants.
 * Running jobs retain their captured policy and source. Log pages are UTF-8
 * byte ranges; leaving a run retires its replies without cancelling the job. */
import { useEffect, useRef, useState } from "react";
import {
  ExternalLink,
  Hammer,
  RefreshCw,
  Square,
  Terminal,
} from "lucide-react";
import {
  experimentalCall,
  type BuildLogView,
  type BuildRunView,
  type JobView,
} from "../../experimentalTypes";
import { useCode } from "../controller";
import { isActiveJob, projectJobOwner } from "../harnessView";
import { label } from "../locale";
import { useProjectJobs } from "./useProjectJobs";
import "./build.css";

const LOG_PAGE_BYTES = 16000;
const runningBuild = (status: string) => status === "running";
function statusLabel(status: string) {
  const names: Record<string, [string, string]> = {
    running: ["构建中", "Building"],
    completed: ["已完成", "Completed"],
    failed: ["构建失败", "Build failed"],
    cancelled: ["已停止", "Stopped"],
    timed_out: ["已超时", "Timed out"],
    output_limit: ["输出达到限额", "Output limit reached"],
    blocked: ["无法构建", "Build unavailable"],
    interrupted: ["已中断", "Interrupted"],
  };
  return names[status] ? label(...names[status]) : status;
}

export function BuildPolicy() {
  const c = useCode(),
    scope = c.projectId,
    current = useRef(scope),
    alive = useRef(true),
    pending = useRef(new Set<string>()),
    [saving, setSaving] = useState(""),
    [error, setError] = useState({ scope: "", message: "" }),
    policy = c.project?.buildPolicy ?? { enabled: false, network: false };
  current.current = scope;
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  async function save(enabled: boolean, network: boolean) {
    if (!c.native || !c.project || pending.current.has(scope)) return;
    pending.current.add(scope);
    setSaving(scope);
    setError({ scope, message: "" });
    try {
      await c.write("project_build_policy", {
        projectId: scope,
        enabled,
        network: enabled && network,
      });
    } catch (error) {
      if (alive.current && current.current === scope)
        setError({ scope, message: String(error) });
    } finally {
      pending.current.delete(scope);
      if (alive.current) setSaving((owner) => (owner === scope ? "" : owner));
    }
  }
  const disabled =
    !c.native ||
    !c.project ||
    !!c.busy ||
    saving === scope ||
    pending.current.has(scope);
  return (
    <section
      className="rc-build-policy"
      aria-label={label("项目构建授权", "Project build permission")}
    >
      <label>
        <input
          type="checkbox"
          checked={policy.enabled}
          disabled={disabled}
          onChange={(event) => void save(event.target.checked, policy.network)}
        />
        {label("允许 AI 构建项目副本", "Allow AI to build a project copy")}
      </label>
      <label>
        <input
          type="checkbox"
          checked={policy.enabled && policy.network}
          disabled={disabled || !policy.enabled}
          onChange={(event) => void save(policy.enabled, event.target.checked)}
        />
        {label("允许构建联网（下载依赖等）", "Allow build network access")}
      </label>
      <p className="rc-muted">
        {label(
          "授权仅作用于后续任务。构建在独立副本中执行，原项目改动仍需审阅。",
          "Permission applies to future tasks. Builds run in a separate copy; changes to the original project still require review.",
        )}
      </p>
      {error.scope === scope && error.message && (
        <p className="rc-harness-error" role="alert">
          {error.message}
        </p>
      )}
    </section>
  );
}

export function BuildRunLog({ run }: { run: BuildRunView }) {
  const c = useCode(),
    scope = c.projectId + ":" + run.jobId + ":" + run.runId,
    [selection, setSelection] = useState<{
      scope: string;
      offset: number | "tail";
    }>({ scope: "", offset: "tail" }),
    [page, setPage] = useState<{
      scope: string;
      offset: number;
      value: BuildLogView;
    } | null>(null),
    [error, setError] = useState({ scope: "", message: "" }),
    [loading, setLoading] = useState(""),
    [retry, setRetry] = useState(0),
    offset = selection.scope === scope ? selection.offset : "tail",
    liveVersion =
      offset === "tail" ? `${run.revision}:${run.logBytes}:${run.status}` : "";
  useEffect(() => {
    let alive = true;
    const start =
      offset === "tail" ? Math.max(0, run.logBytes - LOG_PAGE_BYTES) : offset;
    setLoading(scope);
    setError({ scope, message: "" });
    void experimentalCall<BuildLogView>(c.api, "harness_build_log", {
      jobId: run.jobId,
      runId: run.runId,
      offset: start,
      limit: LOG_PAGE_BYTES,
    })
      .then((value) => {
        if (alive) setPage({ scope, offset: start, value });
      })
      .catch((error) => {
        if (alive) setError({ scope, message: String(error) });
      })
      .finally(() => {
        if (alive) setLoading("");
      });
    return () => {
      alive = false;
    };
  }, [c.api, scope, offset, liveVersion, retry]);
  const visible = page?.scope === scope ? page : null,
    busy = loading === scope,
    message = error.scope === scope ? error.message : "";
  return (
    <div className="rc-build-log">
      {message && (
        <p className="rc-harness-error" role="alert">
          {message}
        </p>
      )}
      {run.logTruncated && (
        <p className="rc-muted">
          {label(
            "日志达到限额，部分输出已省略。",
            "The log limit was reached; some output was omitted.",
          )}
        </p>
      )}
      <pre aria-label={label("构建日志", "Build log")} aria-busy={busy}>
        {visible?.value.content ||
          (busy
            ? label("读取日志…", "Loading log…")
            : label("暂无构建输出", "No build output yet"))}
      </pre>
      {visible?.value.truncated && (
        <p className="rc-muted">
          {label("显示部分日志", "Showing part of the log")}
        </p>
      )}
      <div className="rc-row">
        <button
          className="rc-button"
          disabled={busy || !visible || visible.offset === 0}
          onClick={() =>
            visible &&
            setSelection({
              scope,
              offset: Math.max(0, visible.offset - LOG_PAGE_BYTES),
            })
          }
        >
          {label("上一段", "Previous")}
        </button>
        <button
          className="rc-button"
          disabled={
            busy ||
            !visible ||
            visible.value.nextOffset === null ||
            visible.value.nextOffset >= visible.value.totalBytes
          }
          onClick={() =>
            visible &&
            visible.value.nextOffset !== null &&
            setSelection({ scope, offset: visible.value.nextOffset })
          }
        >
          {label("下一段", "Next")}
        </button>
        <button
          className="rc-button"
          disabled={busy}
          aria-pressed={offset === "tail"}
          onClick={() => {
            setSelection({ scope, offset: "tail" });
            setRetry((value) => value + 1);
          }}
        >
          <RefreshCw size={12} />
          {label("最新日志", "Latest log")}
        </button>
      </div>
    </div>
  );
}

export function BuildRunCard({
  run,
  job,
}: {
  run: BuildRunView;
  job: JobView;
}) {
  const c = useCode(),
    scope = c.projectId + ":" + run.jobId + ":" + run.runId,
    current = useRef(scope),
    alive = useRef(true),
    pending = useRef(false),
    [stopping, setStopping] = useState(false),
    [opening, setOpening] = useState(false),
    [logsOpen, setLogsOpen] = useState(false),
    [error, setError] = useState({ scope: "", message: "" }),
    running = runningBuild(run.status),
    active = isActiveJob(job.summary.status);
  current.current = scope;
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    setStopping(false);
    setOpening(false);
    setLogsOpen(false);
    pending.current = false;
  }, [scope]);
  useEffect(() => {
    if (!running || !active) setStopping(false);
  }, [running, active]);
  async function stop() {
    if (pending.current || stopping || !active || !running) return;
    pending.current = true;
    setStopping(true);
    setError({ scope, message: "" });
    try {
      await c.stop(run.jobId);
    } catch (error) {
      if (alive.current && current.current === scope) {
        setError({ scope, message: String(error) });
        setStopping(false);
      }
    } finally {
      if (current.current === scope) pending.current = false;
    }
  }
  async function openArtifacts() {
    if (pending.current || opening || running || !run.artifacts.length) return;
    const projectId = c.projectId;
    pending.current = true;
    setOpening(true);
    setError({ scope, message: "" });
    try {
      await c.api("rh_code_build_open", {
        args: { projectId, jobId: run.jobId, runId: run.runId },
      });
    } catch (error) {
      if (alive.current && current.current === scope)
        setError({ scope, message: String(error) });
    } finally {
      if (alive.current && current.current === scope) {
        pending.current = false;
        setOpening(false);
      }
    }
  }
  if (run.jobId !== job.summary.jobId)
    return (
      <p className="rc-harness-error" role="alert">
        {label("构建任务身份不匹配", "Build task identity mismatch")}
      </p>
    );
  return (
    <article className="rc-build-run" data-status={run.status}>
      <header>
        <Hammer size={14} />
        <strong>
          {run.recipe} · {run.target}
        </strong>
        <span>
          {stopping ? label("正在停止…", "Stopping…") : statusLabel(run.status)}
        </span>
      </header>
      <div className="rc-build-source">
        <span>
          {run.proposalId
            ? label("建议副本", "Proposal copy")
            : label("已保存源码", "Saved source")}
        </span>
        <code title={run.sourceFingerprint}>
          {run.sourceFingerprint.slice(0, 12)}
        </code>
        {run.exitCode != null && (
          <span>
            {label("退出码", "Exit code")} {run.exitCode}
          </span>
        )}
      </div>
      {run.error?.message && (
        <p className="rc-harness-error" role="alert">
          {run.error.message}
        </p>
      )}
      {error.scope === scope && error.message && (
        <p className="rc-harness-error" role="alert">
          {error.message}
        </p>
      )}
      {active && running && (
        <button
          className="rc-button rc-build-stop"
          disabled={!c.native || stopping}
          onClick={() => void stop()}
        >
          <Square size={12} />
          {label("停止任务", "Stop task")}
        </button>
      )}
      <details onToggle={(event) => setLogsOpen(event.currentTarget.open)}>
        <summary>
          <Terminal size={13} />
          {label("构建日志", "Build log")}
        </summary>
        {logsOpen && <BuildRunLog key={scope} run={run} />}
      </details>
      {run.artifacts.length > 0 && (
        <details>
          <summary>
            {label("产物", "Artifacts")} ·{" "}
            {run.artifactCount ?? run.artifacts.length}
          </summary>
          <ul className="rc-build-artifacts">
            {run.artifacts.map((artifact) => (
              <li key={artifact.path}>
                <code title={artifact.sha256}>{artifact.path}</code>
                <span>{artifact.size.toLocaleString()} B</span>
              </li>
            ))}
          </ul>
          <button
            className="rc-button"
            disabled={!c.native || opening || running}
            onClick={() => void openArtifacts()}
          >
            <ExternalLink size={12} />
            {label("打开产物目录", "Open artifact folder")}
          </button>
        </details>
      )}
      {(run.artifactTruncated ||
        (run.artifactCount ?? 0) > run.artifacts.length) && (
        <p className="rc-muted">
          {label("部分产物未显示", "Some artifacts are not shown")}
        </p>
      )}
    </article>
  );
}

export function BuildPanel() {
  const c = useCode(),
    { jobs, error, refresh } = useProjectJobs(),
    builds = jobs.filter(
      (job) => job.summary.workflow === "harness" && job.buildRuns?.length,
    );
  return (
    <div className="rc-panel-pad rc-build-panel">
      <header className="rc-section-title">
        {label("项目构建", "Project builds")}
      </header>
      <BuildPolicy />
      {error && (
        <div className="rc-harness-error" role="alert">
          <p>{error}</p>
          <button className="rc-button" onClick={refresh}>
            <RefreshCw size={12} />
            {label("重新读取", "Reload")}
          </button>
        </div>
      )}
      {builds.map((job) => (
        <section key={job.summary.jobId}>
          <header className="rc-harness-task-title">
            <strong>
              {c.project &&
                projectJobOwner(c.project, job.summary.jobId)?.title}
            </strong>
            <code>{job.summary.jobId.slice(0, 8)}</code>
          </header>
          {[...job.buildRuns!].reverse().map((run) => (
            <BuildRunCard
              key={c.projectId + ":" + job.summary.jobId + ":" + run.runId}
              run={run}
              job={job}
            />
          ))}
        </section>
      ))}
      {!builds.length && !error && (
        <p className="rc-muted">
          {label(
            "授权后，在执行模式中让 AI 构建项目；日志与产物会显示在这里。",
            "After granting permission, ask AI to build in Execute mode. Logs and artifacts appear here.",
          )}
        </p>
      )}
    </div>
  );
}
