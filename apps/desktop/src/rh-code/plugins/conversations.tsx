import {
  ArrowUp,
  MessageSquare,
  Plus,
  Settings2,
  Square,
  Sparkles,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { CeSelect } from "../../CeSelect";
import { useCode } from "../controller";
import { label } from "../locale";
import type { Area, Mode } from "../types";
import {
  isActiveJob,
  jobStatusLabel,
  jobText,
  modeDescription,
} from "../harnessView";
import { HarnessProposalReview } from "./HarnessProposalReview";
import { JobActivity } from "./JobActivity";
import { useProjectJobs } from "./useProjectJobs";

function RuntimeNotice() {
  const c = useCode();
  const configured = c.engine?.aiPresets.presets.some(
    (preset) => preset.id === c.engine?.aiPresets.selectedId,
  );
  if (!c.native || !c.engine || (configured && c.engine.liveAvailable))
    return null;
  return (
    <div className="rc-harness-runtime" role="status">
      <span>
        {!configured
          ? label(
              "配置 AI 预设后即可开始对话",
              "Configure an AI preset to start a conversation",
            )
          : label("AI 运行环境暂不可用", "The AI runtime is unavailable")}
      </span>
      <button className="rc-button purple" onClick={c.openAiSettings}>
        <Settings2 size={13} />
        {label("配置 API", "Configure API")}
      </button>
    </div>
  );
}
export function Conversations({ area }: { area: Area }) {
  const c = useCode(),
    p = c.project;
  return (
    <div className="rc-panel-pad">
      <header className="rc-section-title">
        {label("项目对话", "Project conversations")}{" "}
        <span>{p?.conversations.length ?? 0}</span>
      </header>
      <div className="rc-thread-list">
        {p?.conversations.map((t) => (
          <button key={t.id} onClick={() => c.openConversation(t.id)}>
            <MessageSquare size={16} />
            <span>
              {t.title}
              <small>
                {t.activeJob
                  ? jobStatusLabel(
                      c.jobs[t.activeJob]?.summary.status ?? "queued",
                    )
                  : t.messages.at(-1)?.content.slice(0, 48) ||
                    label("新对话", "New conversation")}
              </small>
            </span>
          </button>
        ))}
      </div>
      <button
        className="rc-button purple"
        disabled={!c.native || !!c.busy}
        onClick={() => void c.newConversation(area.id).catch(() => {})}
      >
        <Plus size={16} />
        {label("新建对话", "New conversation")}
      </button>
    </div>
  );
}
export function Chat({ area }: { area: Area }) {
  const c = useCode(),
    p = c.project,
    t =
      p?.conversations.find((t) => t.id === area.conversation) ??
      p?.conversations[0];
  const mode: Mode = t?.mode ?? "plan",
    messages = useRef<HTMLDivElement>(null),
    follow = useRef(true),
    currentJob = useRef(t?.activeJob),
    [stopping, setStopping] = useState(false),
    history = useProjectJobs(t?.id),
    active = t?.activeJob
      ? (c.jobs[t.activeJob] ??
        history.jobs.find((job) => job.summary.jobId === t.activeJob))
      : undefined,
    latest = history.jobs[0];
  currentJob.current = t?.activeJob;
  useEffect(() => {
    follow.current = true;
    setStopping(false);
  }, [t?.id]);
  useEffect(() => {
    setStopping(false);
  }, [t?.activeJob]);
  useEffect(() => {
    const node = messages.current;
    if (node && follow.current) node.scrollTop = node.scrollHeight;
  }, [
    t?.id,
    t?.messages.length,
    t?.activeJob,
    active?.summary.revision,
    active && jobText(active),
    active?.activity,
  ]);
  const disabled =
    !c.native ||
    !!c.busy ||
    c.snapshot?.disabledPlugins.includes("rh.agent") ||
    (c.engine &&
      !c.engine.aiPresets.presets.some(
        (preset) => preset.id === c.engine?.aiPresets.selectedId,
      )) ||
    c.engine?.liveAvailable === false;
  async function stop(jobId: string) {
    if (stopping) return;
    setStopping(true);
    try {
      await c.stop(jobId);
    } catch (error) {
      if (currentJob.current === jobId) {
        setStopping(false);
        c.setError(String(error));
      }
      throw error;
    }
  }
  function send() {
    if (!t) return;
    follow.current = true;
    return c.run(area.id, t.id, mode);
  }
  if (!t)
    return (
      <div className="rc-chat-empty">
        <Sparkles size={32} />
        <h2>{label("这一轮，要做什么？", "What shall we make?")}</h2>
        <RuntimeNotice />
        <button
          className="rc-button purple"
          disabled={!c.native || !!c.busy}
          onClick={() => void c.newConversation(area.id).catch(() => {})}
        >
          <Plus size={16} />
          {label("新建对话", "New conversation")}
        </button>
      </div>
    );
  return (
    <div className="rc-chat">
      <div
        className="rc-chat-tabs"
        aria-label={label(
          "项目中的 AI 对话",
          "AI conversations in this project",
        )}
      >
        {p!.conversations.map((v) => (
          <button
            key={v.id}
            aria-pressed={v.id === t.id}
            onClick={() => c.patchArea(area.id, { conversation: v.id })}
          >
            {v.activeJob && <span className="rc-live-dot" />}
            {v.title}
          </button>
        ))}
        <button
          aria-label={label("新建对话", "New conversation")}
          disabled={!c.native || !!c.busy}
          onClick={() => void c.newConversation(area.id).catch(() => {})}
        >
          <Plus size={15} />
        </button>
      </div>
      <RuntimeNotice />
      <div
        className="rc-messages"
        ref={messages}
        onScroll={(event) => {
          const node = event.currentTarget;
          follow.current =
            node.scrollHeight - node.scrollTop - node.clientHeight < 80;
        }}
      >
        {t.messages.length === 0 ? (
          <div className="rc-chat-welcome">
            <Sparkles size={27} />
            <h3>{label("描述这一轮的目标", "Describe this round’s goal")}</h3>
          </div>
        ) : (
          t.messages.map((m) => (
            <article key={m.id} className={"rc-message " + m.role}>
              {m.role === "assistant" && (
                <div className="rc-agent-label">
                  <Sparkles size={15} />
                  RH Agent
                </div>
              )}
              <p>{m.content}</p>
              {m.role === "assistant" &&
                m.jobId &&
                history.jobs.some((job) => job.summary.jobId === m.jobId) && (
                  <JobActivity
                    job={history.jobs.find(
                      (job) => job.summary.jobId === m.jobId,
                    )}
                    jobId={m.jobId}
                    compact
                    showText={false}
                  />
                )}
            </article>
          ))
        )}
        {t.activeJob && (
          <JobActivity job={active} jobId={t.activeJob} onStop={stop} />
        )}
        {latest?.summary.workflow === "harness" && (
          <HarnessProposalReview
            key={c.projectId + ":" + latest.summary.jobId}
            job={latest}
          />
        )}
        {history.error && (
          <p className="rc-harness-error" role="alert">
            {history.error}
            <button className="rc-button" onClick={history.refresh}>
              {label("重新读取", "Reload")}
            </button>
          </p>
        )}
      </div>
      <div className="rc-composer">
        <div className="rc-composer-context">
          <span>{p!.name}</span>
          <span>
            {p!.targets[0]?.minecraft} · {p!.targets[0]?.loaders.join(" + ")}
          </span>
        </div>
        <textarea
          aria-label={label("对话草稿", "Conversation draft") + " · " + t.title}
          placeholder={label("继续这一轮…", "Continue this conversation…")}
          value={c.draft(t.id)}
          onChange={(e) => c.setDraft(t.id, e.target.value)}
          disabled={!c.native}
          onKeyDown={(event) => {
            if (
              event.key === "Enter" &&
              (event.ctrlKey || event.metaKey) &&
              !event.nativeEvent.isComposing &&
              !disabled &&
              !t.activeJob &&
              c.draft(t.id).trim()
            ) {
              event.preventDefault();
              void send();
            }
          }}
        />
        <footer>
          <CeSelect
            value={mode}
            disabled={!c.native || !!c.busy || !!t.activeJob}
            aria-label={label("对话模式", "Conversation mode")}
            onChange={(e) =>
              void c
                .write("conversation_draft", {
                  projectId: c.projectId,
                  conversationId: t.id,
                  draft: c.draft(t.id),
                  mode: e.target.value,
                })
                .catch(() => {})
            }
          >
            <option value="ask">{label("询问", "Ask")}</option>
            <option value="plan">{label("规划", "Plan")}</option>
            <option value="execute">{label("执行", "Execute")}</option>
          </CeSelect>
          <span className="rc-harness-mode" title={modeDescription(mode)}>
            {modeDescription(mode)}
          </span>
          <span className="rc-spacer" />
          {t.activeJob ? (
            <button
              className="rc-send"
              aria-label={label("停止任务", "Stop task")}
              title={
                stopping
                  ? label("正在停止…", "Stopping…")
                  : label("停止任务", "Stop task")
              }
              disabled={
                stopping || (active && !isActiveJob(active.summary.status))
              }
              onClick={() => void stop(t.activeJob!).catch(() => {})}
            >
              <Square size={16} />
            </button>
          ) : (
            <button
              className="rc-send"
              aria-label={label("发送需求", "Send request")}
              title={label(
                "发送需求 · Ctrl+Enter",
                "Send request · Ctrl+Enter",
              )}
              disabled={disabled || !c.draft(t.id).trim()}
              onClick={() => void send()}
            >
              <ArrowUp size={19} />
            </button>
          )}
        </footer>
      </div>
    </div>
  );
}
