import { useEffect, useRef, useState } from "react";
import {
  Check,
  CircleAlert,
  LoaderCircle,
  Square,
  Terminal,
  Wrench,
} from "lucide-react";
import type { JobView } from "../../experimentalTypes";
import { isActiveJob, jobStatusLabel, jobText } from "../harnessView";
import { label } from "../locale";

export function JobActivity({
  job,
  jobId,
  onStop,
  showText = true,
  compact = false,
}: {
  job?: JobView;
  jobId: string;
  onStop?: (id: string) => Promise<unknown>;
  showText?: boolean;
  compact?: boolean;
}) {
  const [stopping, setStopping] = useState(false),
    [error, setError] = useState(""),
    current = useRef(jobId),
    tools = useRef<HTMLOListElement>(null),
    followTools = useRef(true),
    status = job?.summary.status ?? "queued",
    running = isActiveJob(status),
    text = job && showText ? jobText(job) : "",
    activity = job?.activity ?? [];
  current.current = jobId;
  useEffect(() => {
    setStopping(false);
    setError("");
    followTools.current = true;
  }, [jobId]);
  useEffect(() => {
    if (!running) setStopping(false);
  }, [running]);
  useEffect(() => {
    if (tools.current && followTools.current)
      tools.current.scrollTop = tools.current.scrollHeight;
  }, [jobId, activity]);
  async function stop() {
    if (!onStop || stopping) return;
    const ownedId = jobId;
    setStopping(true);
    setError("");
    try {
      await onStop(ownedId);
    } catch (error) {
      if (current.current === ownedId) {
        setError(String(error));
        setStopping(false);
      }
    }
  }
  return (
    <section
      className={"rc-harness-activity" + (compact ? " compact" : "")}
      aria-label={label("Agent 任务活动", "Agent task activity")}
    >
      <header>
        {running ? (
          <LoaderCircle size={14} className="rc-harness-spinner" />
        ) : status === "completed" ? (
          <Check size={14} />
        ) : (
          <CircleAlert size={14} />
        )}
        <strong>
          {stopping ? label("正在停止…", "Stopping…") : jobStatusLabel(status)}
        </strong>
        <code title={jobId}>{jobId.slice(0, 8)}</code>
        {running && onStop && (
          <button
            className="rc-button rc-harness-stop"
            disabled={stopping}
            onClick={() => void stop()}
          >
            <Square size={12} />
            {label("停止", "Stop")}
          </button>
        )}
      </header>
      {(job?.error?.message || error) && (
        <p className="rc-harness-error" role="alert">
          {error || job?.error?.message}
        </p>
      )}
      {status === "interrupted" && !job?.error?.message && (
        <p className="rc-harness-error">
          {label(
            "任务已中断，可以继续发送需求。",
            "The task was interrupted. You can send a new request.",
          )}
        </p>
      )}
      {activity.length > 0 && (
        <details className="rc-harness-tools" open={running || !compact}>
          <summary>
            <Wrench size={13} />
            {label("工具活动", "Tool activity")}
            <span>{activity.length}</span>
          </summary>
          <ol
            ref={tools}
            onScroll={(event) => {
              const node = event.currentTarget;
              followTools.current =
                node.scrollHeight - node.scrollTop - node.clientHeight < 40;
            }}
          >
            {activity.map((tool) => (
              <li key={tool.id} data-status={tool.status}>
                <div>
                  <code>{tool.name}</code>
                  <span>{jobStatusLabel(tool.status)}</span>
                </div>
                {tool.path && (
                  <code className="rc-harness-path">{tool.path}</code>
                )}
                {tool.steps && (
                  <ul>
                    {tool.steps.map((step, index) => (
                      <li key={index}>{step}</li>
                    ))}
                  </ul>
                )}
              </li>
            ))}
          </ol>
        </details>
      )}
      {text && (
        <div className="rc-harness-output">
          <span>
            <Terminal size={13} />
            {label("模型输出", "Model output")}
          </span>
          <p>{text}</p>
        </div>
      )}
      {running && !text && !activity.length && (
        <p className="rc-harness-wait" role="status">
          {label(
            "正在等待模型与工具回执…",
            "Waiting for model and tool activity…",
          )}
        </p>
      )}
    </section>
  );
}
