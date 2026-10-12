/** Compact conversation metadata controls. Text and selected references live in
 * the controller; this view owns only disclosures and scoped request feedback. */
import {
  Check,
  FileCode2,
  Paperclip,
  Pencil,
  Settings2,
  X,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useCode } from "../controller";
import { label } from "../locale";
import type { Conversation } from "../types";

const REFERENCE_LIMIT = 20;
const MATCH_LIMIT = 40;
const excluded = new Set([
  "node_modules",
  "target",
  "build",
  "dist",
  "vendor",
  "venv",
  "__pycache__",
]);
// The inventory predates Harness capture. Hide entries the native Harness path
// contract excludes; native capture still checks existence and frozen text.
export function contextReferencePaths(files: readonly string[]) {
  return [
    ...new Set(
      files.filter(
        (path) =>
          typeof path === "string" &&
          !!path &&
          new TextEncoder().encode(path).length <= 512 &&
          !/[\u0000-\u001f\u007f-\u009f\\:]/u.test(path) &&
          path
            .split("/")
            .every(
              (part) => !!part && !part.startsWith(".") && !excluded.has(part),
            ),
      ),
    ),
  ].sort();
}

export function ConversationName({
  conversation,
}: {
  conversation: Conversation;
}) {
  const c = useCode(),
    scope = c.projectId + ":" + conversation.id,
    current = useRef(scope),
    [opened, setOpened] = useState(false),
    [saving, setSaving] = useState(false),
    [error, setError] = useState("");
  current.current = scope;
  useEffect(() => {
    setOpened(false);
    setSaving(false);
    setError("");
  }, [scope]);
  useEffect(
    () => () => {
      current.current = "";
    },
    [],
  );
  async function save() {
    if (saving) return;
    const owner = scope;
    setSaving(true);
    setError("");
    try {
      await c.saveConversationTitle(c.projectId, conversation.id);
      if (current.current === owner && !c.titleDirty(conversation.id))
        setOpened(false);
    } catch (error) {
      if (current.current === owner) setError(String(error));
    } finally {
      if (current.current === owner) setSaving(false);
    }
  }
  return (
    <div className="rc-conversation-name">
      <button
        className="rc-icon-button"
        aria-label={label("重命名对话", "Rename conversation")}
        aria-expanded={opened}
        disabled={!c.native}
        onClick={() => setOpened((value) => !value)}
      >
        <Pencil size={13} />
      </button>
      {opened && (
        <div className="rc-name-editor">
          <input
            autoFocus
            aria-label={label("对话名称", "Conversation name")}
            value={c.conversationTitle(conversation.id)}
            maxLength={120}
            disabled={!c.native}
            onChange={(event) =>
              c.setConversationTitle(conversation.id, event.target.value)
            }
            onKeyDown={(event) => {
              if (
                event.key === "Enter" &&
                !event.nativeEvent.isComposing &&
                c.conversationTitle(conversation.id).trim()
              ) {
                event.preventDefault();
                void save();
              }
            }}
          />
          <button
            className="rc-icon-button"
            aria-label={
              saving
                ? label("正在保存…", "Saving…")
                : label("保存名称", "Save name")
            }
            disabled={
              !c.native ||
              saving ||
              !c.titleDirty(conversation.id) ||
              !c.conversationTitle(conversation.id).trim()
            }
            onClick={() => void save()}
          >
            <Check size={14} />
          </button>
          <button
            className="rc-icon-button"
            aria-label={label("收起", "Collapse")}
            onClick={() => setOpened(false)}
          >
            <X size={14} />
          </button>
          {error && (
            <span className="rc-context-error" role="alert">
              {error}
            </span>
          )}
        </div>
      )}
      {c.titleDirty(conversation.id) && (
        <span
          className="rc-context-dirty"
          title={label("名称未保存", "Name is unsaved")}
        >
          ·
        </span>
      )}
    </div>
  );
}

export function ConversationContext({
  conversation,
}: {
  conversation: Conversation;
}) {
  const c = useCode(),
    id = conversation.id,
    scope = c.projectId + ":" + id,
    current = useRef(scope),
    [editor, setEditor] = useState(false),
    [picker, setPicker] = useState(false),
    [query, setQuery] = useState(""),
    [inventory, setInventory] = useState<{
      scope: string;
      paths: string[];
    } | null>(null),
    [loading, setLoading] = useState(false),
    [saving, setSaving] = useState(false),
    [checking, setChecking] = useState(""),
    [error, setError] = useState(""),
    context = c.conversationContext(id),
    instructions = c.projectInstructions(),
    dirty = c.contextDirty(id) || c.instructionsDirty(),
    fileDisabled =
      !c.native || c.snapshot?.disabledPlugins.includes("rh.files");
  current.current = scope;
  useEffect(() => {
    setEditor(false);
    setPicker(false);
    setQuery("");
    setSaving(false);
    setChecking("");
    setError("");
  }, [scope]);
  useEffect(
    () => () => {
      current.current = "";
    },
    [],
  );
  useEffect(() => {
    let alive = true;
    if (!picker || fileDisabled) return;
    setLoading(true);
    setError("");
    void c
      .api<{ files: string[] }>("rh_code_call", {
        operation: "files",
        args: { projectId: c.projectId },
      })
      .then((value) => {
        if (alive)
          setInventory({ scope, paths: contextReferencePaths(value.files) });
      })
      .catch((error) => {
        if (alive) setError(String(error));
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [c.api, c.native, scope, c.filesEpoch, picker, fileDisabled]);
  async function save() {
    if (saving) return;
    const owner = scope;
    setSaving(true);
    setError("");
    try {
      // Admit both submitted fields now. The controller serializes their CAS
      // writes; typing while the first field saves remains a newer draft.
      const results = await Promise.allSettled([
        c.saveConversationContext(c.projectId, id),
        c.saveProjectInstructions(c.projectId),
      ]);
      const failure = results.find((result) => result.status === "rejected");
      if (failure?.status === "rejected") throw failure.reason;
    } catch (error) {
      if (current.current === owner) setError(String(error));
    } finally {
      if (current.current === owner) setSaving(false);
    }
  }
  async function reference(path: string) {
    if (checking || fileDisabled) return;
    const owner = scope;
    setChecking(path);
    setError("");
    try {
      // Inventory includes binary files. Check the selected entry through the
      // existing bounded text reader before persisting the reference.
      const value = await c.api<{ content: string }>("rh_code_call", {
        operation: "file_read",
        args: { projectId: c.projectId, path },
      });
      if (current.current !== owner) return;
      if (typeof value.content !== "string" || value.content.includes("\0"))
        throw new Error(
          label("请选择 UTF-8 文本文件", "Choose a UTF-8 text file"),
        );
      const next = c.conversationContext(id);
      if (
        next.contextPaths.includes(path) ||
        next.contextPaths.length >= REFERENCE_LIMIT
      )
        return;
      c.setConversationContext(id, {
        ...next,
        contextPaths: [...next.contextPaths, path],
      });
      await c.saveConversationContext(c.projectId, id);
    } catch (error) {
      if (current.current === owner) setError(String(error));
    } finally {
      if (current.current === owner) setChecking("");
    }
  }
  async function remove(path: string) {
    const owner = scope;
    const next = c.conversationContext(id);
    c.setConversationContext(id, {
      ...next,
      contextPaths: next.contextPaths.filter((entry) => entry !== path),
    });
    try {
      await c.saveConversationContext(c.projectId, id);
    } catch (error) {
      if (current.current === owner) setError(String(error));
    }
  }
  const available = inventory?.scope === scope ? inventory.paths : [],
    matches = available.filter((path) =>
      path.toLocaleLowerCase().includes(query.toLocaleLowerCase()),
    ),
    shown = matches.slice(0, MATCH_LIMIT);
  return (
    <div className="rc-conversation-context">
      <div className="rc-context-toolbar">
        <button
          className="rc-context-toggle blue"
          aria-label={label("引用项目文件", "Reference project files")}
          aria-expanded={picker}
          disabled={fileDisabled}
          onClick={() => {
            setPicker((value) => !value);
            setEditor(false);
          }}
        >
          <Paperclip size={13} />
          {label("引用", "Files")}
          <small>{context.contextPaths.length}/20</small>
        </button>
        <button
          className="rc-context-toggle purple"
          aria-expanded={editor}
          onClick={() => {
            setEditor((value) => !value);
            setPicker(false);
          }}
        >
          <Settings2 size={13} />
          {label("上下文", "Context")}
          {(context.contextNote || instructions) && (
            <span className="rc-context-present">·</span>
          )}
        </button>
        {dirty && (
          <span className="rc-context-dirty">{label("未保存", "Unsaved")}</span>
        )}
        {(dirty || saving) && (
          <button
            className="rc-context-save"
            disabled={!c.native || saving}
            onClick={() => void save()}
          >
            {saving ? label("保存中…", "Saving…") : label("保存", "Save")}
          </button>
        )}
      </div>
      {!!context.contextPaths.length && (
        <div
          className="rc-context-chips"
          aria-label={label("对话文件引用", "Conversation file references")}
        >
          {context.contextPaths.map((path) => (
            <span key={path} title={path}>
              <FileCode2 size={12} />
              <code>{path}</code>
              <button
                aria-label={
                  label("移除引用", "Remove reference") + " · " + path
                }
                disabled={!c.native}
                onClick={() => void remove(path)}
              >
                <X size={12} />
              </button>
            </span>
          ))}
        </div>
      )}
      {editor && (
        <div className="rc-context-editor">
          <label>
            <span>
              {label("对话笔记 / 长期目标", "Conversation note / goal")}
              <small>{Array.from(context.contextNote).length}/8000</small>
            </span>
            <textarea
              aria-label={label(
                "对话笔记 / 长期目标",
                "Conversation note / goal",
              )}
              value={context.contextNote}
              maxLength={8000}
              disabled={!c.native}
              placeholder={label(
                "此对话需要记住的目标与约定",
                "Goals and decisions for this conversation",
              )}
              onChange={(event) =>
                c.setConversationContext(id, {
                  ...c.conversationContext(id),
                  contextNote: event.target.value,
                })
              }
            />
          </label>
          <label>
            <span>
              {label("项目指令", "Project instructions")}
              <small>{Array.from(instructions).length}/8000</small>
            </span>
            <textarea
              aria-label={label("项目指令", "Project instructions")}
              value={instructions}
              maxLength={8000}
              disabled={!c.native}
              placeholder={label(
                "本项目所有对话共用的约定",
                "Shared instructions for this project",
              )}
              onChange={(event) => c.setProjectInstructions(event.target.value)}
            />
          </label>
        </div>
      )}
      {picker && (
        <div className="rc-context-picker">
          <input
            autoFocus
            aria-label={label("搜索项目文件", "Search project files")}
            placeholder={label("搜索项目文件…", "Search project files…")}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
          <div className="rc-context-matches">
            {shown.map((path) => (
              <button
                key={path}
                title={path}
                aria-pressed={context.contextPaths.includes(path)}
                disabled={
                  !!checking ||
                  context.contextPaths.includes(path) ||
                  context.contextPaths.length >= REFERENCE_LIMIT
                }
                onClick={() => void reference(path)}
              >
                <FileCode2 size={12} />
                <code>{path}</code>
                {context.contextPaths.includes(path) && <Check size={12} />}
              </button>
            ))}
            {!shown.length && (
              <span>
                {loading
                  ? label("读取中…", "Loading…")
                  : label("没有匹配文件", "No matching files")}
              </span>
            )}
          </div>
          <small>
            {matches.length > MATCH_LIMIT
              ? label("继续输入以缩小结果", "Type more to narrow the results")
              : label("项目文本文件", "Project text files")}
          </small>
        </div>
      )}
      {error && (
        <p className="rc-context-error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
