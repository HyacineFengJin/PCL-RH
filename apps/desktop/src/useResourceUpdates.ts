import { useEffect, useMemo, useRef, useState } from "react";
import type { Api } from "./types";
import { useInstanceOperationScope } from "./instanceOperationUi";
import {
  checkResourceUpdates,
  resourceUpdateFiles,
  type ResourceFile,
  type ResourceUpdateCheck,
  type ResourceUpdateHistory,
  type ResourceUpdateTarget,
} from "./resourceUpdateTypes";

type Inspection = {
  scope: object;
  phase: "checking" | "ready" | "error";
  result: ResourceUpdateCheck | null;
  error: string;
};
type HistoryState = {
  scope: object;
  entries: ResourceUpdateHistory[];
  error: string;
};
export type ResourceUpdateChoice = { scope: object; files: ResourceFile[] };

/** One owner binds inspection/history/selection to the root, target and resource
 * scan generation. No saved handler or late task response can adopt a new view. */
export function useResourceUpdates({
  api,
  scopeKey,
  instance,
  generation,
  files,
  enabled,
  native,
  disabled,
  onTaskStart,
  onNotify,
}: {
  api: Api;
  scopeKey: string;
  instance: ResourceUpdateTarget;
  generation: string;
  files: ResourceFile[];
  enabled: boolean;
  native: boolean;
  disabled: boolean;
  onTaskStart: (id: string) => void;
  onNotify: (message: string) => void;
}) {
  const root = useInstanceOperationScope(api, scopeKey, native, disabled);
  // The list owns immutable scan snapshots. Selection/progress renders retain
  // that snapshot, so they need neither another sort nor another large string.
  // Actual scope/generation/content changes still retire the inspection owner.
  const identity = useMemo(
    () =>
      JSON.stringify([
        instance.id,
        instance.minecraft_version,
        instance.loader,
        generation,
        enabled,
        [...files]
          .sort((a, b) => a.file_name.localeCompare(b.file_name))
          .map((file) => [file.file_name, file.fingerprint]),
      ]),
    [
      instance.id,
      instance.minecraft_version,
      instance.loader,
      generation,
      enabled,
      files,
    ],
  );
  const owner = useRef({ root: root.scope, identity });
  if (owner.current.root !== root.scope || owner.current.identity !== identity)
    owner.current = { root: root.scope, identity };
  const scope = owner.current;
  const current = () => root.current() && owner.current === scope;
  const allowed = () => current() && root.allowed() && enabled && !!scopeKey;
  const [inspection, setInspection] = useState<Inspection | null>(null);
  const [history, setHistory] = useState<HistoryState | null>(null);
  const [choice, setChoice] = useState<ResourceUpdateChoice | null>(null);
  const [restoring, setRestoring] = useState<object | null>(null);
  const inspectionRef = useRef(inspection),
    historyRef = useRef(history);
  inspectionRef.current = inspection;
  historyRef.current = history;
  const checking = useRef<{ scope: object } | null>(null),
    historyRead = useRef<{ scope: object } | null>(null),
    restore = useRef<{ scope: object } | null>(null);
  const initial = useRef<object | null>(null);
  const callbacks = useRef({ onTaskStart, onNotify });
  callbacks.current = { onTaskStart, onNotify };
  const visible = inspection?.scope === scope ? inspection : null;
  const visibleHistory = history?.scope === scope ? history : null;

  async function check() {
    if (
      !allowed() ||
      checking.current?.scope === scope ||
      restore.current?.scope === scope
    )
      return;
    const token = { scope };
    checking.current = token;
    setChoice(null);
    setInspection({ scope, phase: "checking", result: null, error: "" });
    try {
      const result = await api<ResourceUpdateCheck>("resource_update_check", {
        id: instance.id,
      });
      if (!current() || checking.current !== token) return;
      checkResourceUpdates(result, scopeKey, instance, files);
      setInspection({ scope, phase: "ready", result, error: "" });
    } catch (error) {
      if (current() && checking.current === token)
        setInspection({
          scope,
          phase: "error",
          result: null,
          error: String(error),
        });
    } finally {
      if (checking.current === token) checking.current = null;
    }
  }
  async function readHistory() {
    if (
      !allowed() ||
      historyRead.current?.scope === scope ||
      restore.current?.scope === scope
    )
      return;
    const token = { scope };
    historyRead.current = token;
    try {
      const entries = await api<ResourceUpdateHistory[]>(
        "resource_update_history",
        { id: instance.id },
      );
      if (!current() || historyRead.current !== token) return;
      setHistory({
        scope,
        entries: [...entries].sort((a, b) => b.created_at - a.created_at),
        error: "",
      });
    } catch (error) {
      if (current() && historyRead.current === token)
        setHistory({ scope, entries: [], error: String(error) });
    } finally {
      if (historyRead.current === token) historyRead.current = null;
    }
  }
  useEffect(() => {
    let disposed = false;
    queueMicrotask(() => {
      if (disposed || !allowed() || initial.current === scope) return;
      initial.current = scope;
      void check();
      void readHistory();
    });
    return () => {
      disposed = true;
    };
  }, [scope, native, disabled]);

  function open(selected: ResourceFile[]) {
    const result =
      inspectionRef.current?.scope === scope
        ? inspectionRef.current.result
        : null;
    if (
      !allowed() ||
      !result ||
      checking.current?.scope === scope ||
      restore.current?.scope === scope ||
      !selected.length
    )
      return;
    if (
      selected.some(
        (file) =>
          !result.entries.some(
            (entry) =>
              entry.file_name === file.file_name &&
              entry.fingerprint === file.fingerprint &&
              entry.status === "update_available",
          ),
      )
    )
      return;
    setChoice({ scope, files: resourceUpdateFiles(selected) });
  }
  function close() {
    if (current()) setChoice(null);
  }
  async function undo(entry: ResourceUpdateHistory) {
    if (
      !allowed() ||
      restore.current?.scope === scope ||
      checking.current?.scope === scope ||
      historyRef.current?.scope !== scope ||
      historyRef.current.error ||
      !historyRef.current.entries.includes(entry)
    )
      return;
    const token = { scope };
    restore.current = token;
    setRestoring(scope);
    setChoice(null);
    try {
      const task = await api<{ id: string }>("resource_update_restore", {
        id: instance.id,
        undoId: entry.id,
      });
      if (current() && restore.current === token)
        callbacks.current.onTaskStart(task.id);
    } catch (error) {
      if (current() && restore.current === token) {
        const message = String(error);
        setHistory((previous) =>
          previous?.scope === scope
            ? { ...previous, error: message }
            : previous,
        );
        callbacks.current.onNotify(message);
      }
    } finally {
      if (restore.current === token) {
        restore.current = null;
        if (current()) setRestoring(null);
      }
    }
  }
  return {
    scope,
    current,
    phase: visible?.phase ?? "idle",
    result: visible?.result ?? null,
    error: visible?.error ?? "",
    history: visibleHistory?.entries ?? [],
    historyError: visibleHistory?.error ?? "",
    restoring: restoring === scope,
    busy: visible?.phase === "checking" || restoring === scope,
    choice: choice?.scope === scope ? choice : null,
    check,
    readHistory,
    open,
    close,
    undo,
  };
}
