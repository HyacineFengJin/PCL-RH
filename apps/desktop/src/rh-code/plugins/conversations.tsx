import { ArrowUp, MessageSquare, Plus, Square, Sparkles } from "lucide-react";
import { useEffect, useRef } from "react";
import { CeSelect } from "../../CeSelect";
import { useCode } from "../controller";
import { label } from "../locale";
import type { Area, Mode } from "../types";
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
                  ? label("进行中", "Running")
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
    messages = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const node = messages.current;
    if (node) node.scrollTop = node.scrollHeight;
  }, [t?.id, t?.messages.length]);
  const disabled =
    !c.native || !!c.busy || c.snapshot?.disabledPlugins.includes("rh.agent");
  if (!t)
    return (
      <div className="rc-chat-empty">
        <Sparkles size={32} />
        <h2>{label("这一轮，要做什么？", "What shall we make?")}</h2>
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
      <div className="rc-messages" ref={messages}>
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
            </article>
          ))
        )}
        {t.activeJob && (
          <div className="rc-agent-wait">
            <span className="rc-live-dot" />
            {label("Agent 正在工作", "Agent is working")}
          </div>
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
        />
        <footer>
          <CeSelect
            value={mode}
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
          <span className="rc-spacer" />
          {t.activeJob ? (
            <button
              className="rc-send"
              aria-label={label("停止任务", "Stop task")}
              onClick={() =>
                void c.stop(t.activeJob!).catch((e) => c.setError(String(e)))
              }
            >
              <Square size={16} />
            </button>
          ) : (
            <button
              className="rc-send"
              aria-label={label("发送需求", "Send request")}
              disabled={disabled || !c.draft(t.id).trim()}
              onClick={() => void c.run(area.id, t.id, mode)}
            >
              <ArrowUp size={19} />
            </button>
          )}
        </footer>
      </div>
    </div>
  );
}
