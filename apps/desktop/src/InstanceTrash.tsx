import { t, formatNumber } from "./i18n";
import { useEffect, useRef, useState } from "react";
import type { Api, InstanceDeletedEntry, InstanceDeletePlan } from "./types";
import {
  InstanceOperationDialog,
  instanceOperationSize,
  useInstanceOperationScope,
} from "./instanceOperationUi";
import "./instance-operations.css";

type TaskProps = {
  api: Api;
  rootId: string;
  scopeKey: string;
  native: boolean;
  disabled: boolean;
  onTaskStart: (id: string, reveal?: boolean) => void;
};

/** The existing overview action prepares the whole physical instance range.
 * Read cancellation is local; admitted writes move to the shared task manager. */
export function InstanceDelete({
  id,
  api,
  rootId,
  scopeKey,
  native,
  disabled,
  onTaskStart,
}: TaskProps & { id: string }) {
  const { scope, current, allowed } = useInstanceOperationScope(
    api,
    `${rootId}:${scopeKey}:${id}`,
    native,
    disabled,
  );
  const [dialog, setDialogState] = useState<{
    scope: object;
    plan: InstanceDeletePlan | null;
    error: string;
  } | null>(null);
  const dialogRef = useRef(dialog);
  const operation = useRef<{ scope: object; kind: "prepare" | "start" } | null>(
    null,
  );
  const [activity, setActivity] = useState<typeof operation.current>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const callback = useRef(onTaskStart);
  callback.current = onTaskStart;
  function setDialog(next: typeof dialog) {
    dialogRef.current = next;
    setDialogState(next);
  }
  const visible = dialog?.scope === scope ? dialog : null;
  const working = activity?.scope === scope ? activity.kind : null;
  function close() {
    if (
      !current() ||
      (operation.current?.scope === scope && operation.current.kind === "start")
    )
      return;
    operation.current = null;
    setActivity(null);
    setDialog(null);
    trigger.current?.focus();
  }
  async function prepare() {
    if (!allowed() || !rootId || operation.current?.scope === scope) return;
    const token = { scope, kind: "prepare" as const };
    const next = { scope, plan: null, error: "" };
    setDialog(next);
    operation.current = token;
    setActivity(token);
    try {
      const plan = await api<InstanceDeletePlan>("instance_delete_prepare", {
        id,
        rootId,
      });
      if (
        !current() ||
        operation.current !== token ||
        dialogRef.current !== next
      )
        return;
      if (plan.id !== id || plan.root_id !== rootId || !plan.revision)
        throw new Error(t("trash.planChanged"));
      setDialog({ ...next, plan });
    } catch (error) {
      if (
        current() &&
        operation.current === token &&
        dialogRef.current === next
      )
        setDialog({ ...next, error: String(error) });
    } finally {
      if (operation.current === token) {
        operation.current = null;
        if (current()) setActivity(null);
      }
    }
  }
  async function start() {
    const submitted = dialogRef.current;
    if (
      !allowed() ||
      !rootId ||
      !submitted ||
      submitted.scope !== scope ||
      operation.current?.scope === scope
    )
      return;
    if (!submitted.plan) {
      await prepare();
      return;
    }
    if (
      submitted.plan.id !== id ||
      submitted.plan.root_id !== rootId ||
      !submitted.plan.revision
    ) {
      setDialog({ scope, plan: null, error: t("trash.planChanged") });
      return;
    }
    const token = { scope, kind: "start" as const };
    const onSubmittedTask = callback.current;
    operation.current = token;
    setActivity(token);
    try {
      const result = await api<{ id: string }>("instance_delete_start", {
        id: submitted.plan.id,
        revision: submitted.plan.revision,
        rootId,
      });
      if (!result.id) throw new Error(t("trash.taskMissing"));
      const reveal =
        current() &&
        operation.current === token &&
        dialogRef.current === submitted;
      if (reveal) setDialog(null);
      // Native admission owns this task even after navigation or unmounting.
      onSubmittedTask(result.id, reveal);
    } catch (error) {
      // A rejected revision is discarded; a retry must describe a fresh range.
      if (
        current() &&
        operation.current === token &&
        dialogRef.current === submitted
      )
        setDialog({ scope, plan: null, error: String(error) });
    } finally {
      if (operation.current === token) {
        operation.current = null;
        if (current()) setActivity(null);
      }
    }
  }
  return (
    <>
      <button
        ref={trigger}
        className="ce-button danger"
        disabled={!native || disabled || !rootId || !!visible || !!working}
        title={!native ? t("trash.desktop") : t("trash.undoHelp")}
        onClick={() => {
          if (
            !allowed() ||
            dialogRef.current?.scope === scope ||
            operation.current?.scope === scope
          )
            return;
          void prepare();
        }}
      >
        {t("trash.deleteInstance")}
      </button>
      {visible && (
        <InstanceOperationDialog
          title={t("trash.deleteInstance")}
          titleId="ce-instance-delete-title"
          busy={!!working}
          committing={working === "start"}
          confirmLabel={
            working === "start"
              ? t("ui.submitting")
              : working === "prepare"
                ? t("ui.checking")
                : visible.plan
                  ? t("ui.delete")
                  : t("ui.recheck")
          }
          confirmDisabled={!native || disabled}
          onConfirm={() => void start()}
          onClose={close}
        >
          {visible.plan ? (
            <>
              <p>{t("trash.help")}</p>
              <dl>
                <dt>{t("ui.instance")}</dt>
                <dd>{visible.plan.id}</dd>
                <dt>{t("ui.folder")}</dt>
                <dd>versions/{visible.plan.id}</dd>
                <dt>{t("trash.contents")}</dt>
                <dd>
                  {t("ui.fileCountSize", {
                    count: formatNumber(visible.plan.total_files),
                    size: instanceOperationSize(visible.plan.total_bytes),
                  })}
                </dd>
              </dl>
              <p>{t("trash.sharedRetained")}</p>
            </>
          ) : (
            !visible.error && <p role="status">{t("trash.checking")}</p>
          )}
          {visible.error && (
            <p className="rd-name-error" role="alert">
              {visible.error}
            </p>
          )}
        </InstanceOperationDialog>
      )}
    </>
  );
}

/** Persistent undo records stay visible on conflicts. Refresh replaces only the
 * owning root's read snapshot; restore submits the exact record revision. */
export function InstanceTrash({
  api,
  rootId,
  scopeKey,
  native,
  disabled,
  onTaskStart,
  refreshKey = 0,
}: TaskProps & { refreshKey?: string | number }) {
  const { scope, current, readable, allowed } = useInstanceOperationScope(
    api,
    `${rootId}:${scopeKey}`,
    native,
    disabled,
  );
  type Snapshot = {
    scope: object;
    entries: InstanceDeletedEntry[];
    error: string;
    loading: boolean;
  };
  const [snapshot, setSnapshot] = useState<Snapshot>({
    scope,
    entries: [],
    error: "",
    loading: true,
  });
  const latest = useRef(snapshot);
  latest.current = snapshot;
  const readEpoch = useRef(0);
  const operation = useRef<{ scope: object; operationId: string } | null>(null);
  const [activity, setActivity] = useState<typeof operation.current>(null);
  const callback = useRef(onTaskStart);
  callback.current = onTaskStart;
  const visible =
    snapshot.scope === scope
      ? snapshot
      : { scope, entries: [], error: "", loading: true };
  const working = activity?.scope === scope ? activity : null;
  async function read() {
    if (!readable() || !rootId) return;
    const epoch = ++readEpoch.current;
    setSnapshot((old) => ({
      scope,
      entries: old.scope === scope ? old.entries : [],
      error: "",
      loading: true,
    }));
    try {
      const entries = await api<InstanceDeletedEntry[]>(
        "instance_deleted_list",
        { rootId },
      );
      if (!current() || readEpoch.current !== epoch) return;
      if (entries.some((entry) => entry.root_id !== rootId))
        throw new Error(t("trash.recordChanged"));
      setSnapshot({
        scope,
        entries: [...entries].sort((a, b) => b.created_ms - a.created_ms),
        error: "",
        loading: false,
      });
    } catch (error) {
      if (current() && readEpoch.current === epoch)
        setSnapshot((old) => ({
          scope,
          entries: old.scope === scope ? old.entries : [],
          error: String(error),
          loading: false,
        }));
    }
  }
  useEffect(() => {
    void read();
    return () => {
      ++readEpoch.current;
    };
  }, [scope, native, refreshKey]);
  async function restore(entry: InstanceDeletedEntry) {
    const saved =
      latest.current.scope === scope
        ? latest.current.entries.find(
            (candidate) => candidate.operation_id === entry.operation_id,
          )
        : null;
    if (
      !allowed() ||
      !rootId ||
      operation.current?.scope === scope ||
      latest.current.loading ||
      latest.current.error ||
      !saved?.can_restore ||
      saved.root_id !== rootId ||
      entry.root_id !== rootId ||
      !saved.revision ||
      saved.revision !== entry.revision
    )
      return;
    const token = { scope, operationId: entry.operation_id };
    const onSubmittedTask = callback.current;
    operation.current = token;
    setActivity(token);
    try {
      const result = await api<{ id: string }>("instance_restore_start", {
        operationId: saved.operation_id,
        revision: saved.revision,
        rootId,
      });
      if (!result.id) throw new Error(t("trash.restoreTaskMissing"));
      onSubmittedTask(result.id, current() && operation.current === token);
    } catch (error) {
      if (current() && operation.current === token)
        setSnapshot((old) =>
          old.scope === scope ? { ...old, error: String(error) } : old,
        );
    } finally {
      if (operation.current === token) {
        operation.current = null;
        if (current()) setActivity(null);
      }
    }
  }
  if (!native || (!visible.entries.length && !visible.error)) return null;
  return (
    <div className="ce-instance-trash" aria-busy={visible.loading || !!working}>
      {visible.error && (
        <div className="auth-notice ce-instance-trash-error" role="alert">
          <span>{visible.error}</span>
          <button
            className="ce-button"
            disabled={visible.loading || !!working}
            onClick={() => {
              if (current() && operation.current?.scope !== scope) void read();
            }}
          >
            {t("ui.refresh")}
          </button>
        </div>
      )}
      {visible.entries.map((entry) => (
        <div
          className="auth-notice ce-instance-trash-entry"
          key={entry.operation_id}
          role="status"
        >
          <span>
            {entry.state === "deleted"
              ? t("trash.deleted", { name: entry.id })
              : t("trash.incomplete", { name: entry.id })}
            {entry.warning || t("trash.restoreHelp")}
          </span>
          <button
            className="ce-button"
            disabled={
              disabled ||
              visible.loading ||
              !!visible.error ||
              !!working ||
              !entry.can_restore ||
              !entry.revision
            }
            onClick={() => void restore(entry)}
          >
            {working?.operationId === entry.operation_id
              ? t("ui.submitting")
              : t("trash.undo")}
          </button>
          {entry.warning && (
            <button
              className="ce-button"
              disabled={visible.loading || !!working}
              onClick={() => {
                if (current() && operation.current?.scope !== scope)
                  void read();
              }}
            >
              {t("ui.refresh")}
            </button>
          )}
        </div>
      ))}
    </div>
  );
}
