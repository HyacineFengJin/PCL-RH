import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { Api } from "./types";
import { t } from "./i18n";
import {
  emptyTaskView,
  type DownloadStatus,
  type DownloadTaskView,
} from "./taskTypes";
import {
  adoptTaskView,
  taskIdentity,
  taskIsActive,
  taskIsTerminal,
  validTaskView,
} from "./taskLifecycle";
import {
  createLauncherClosingSource,
  type LauncherClosingSource,
} from "./useLauncherMinecraftNotices";

/** One application owner reads the scheduler, regardless of the browsed root.
 * Physical reads stay serial across API/lifetime replacement; a wake requested
 * during an older read is serviced after it finishes. Task events carry no
 * authority. Submission and cancellation callers await a snapshot started after
 * their wake, so an earlier poll cannot claim their new task/page ownership. */
export function useDownloadTasks({
  api,
  native,
  onTerminal,
  subscribeClosing,
}: {
  api: Api;
  native: boolean;
  onTerminal?: (status: DownloadStatus) => void;
  subscribeClosing?: LauncherClosingSource;
}) {
  const [view, setView] = useState<DownloadTaskView>(emptyTaskView),
    [error, setError] = useState(""),
    [isClosing, setClosing] = useState(false);
  const binding = useRef({ api, native, subscribeClosing });
  if (
    binding.current.api !== api ||
    binding.current.native !== native ||
    binding.current.subscribeClosing !== subscribeClosing
  )
    binding.current = { api, native, subscribeClosing };
  const owner = binding.current;
  const snapshot = useRef<DownloadTaskView>(emptyTaskView),
    snapshotOwner = useRef<object | null>(null);
  const physical = useRef<symbol | null>(null),
    wake = useRef<(() => void) | null>(null);
  const request = useRef<(() => Promise<DownloadTaskView | null>) | null>(null);
  const submitted = useRef(new Set<string>()),
    observed = useRef(new Set<string>()),
    terminal = useRef(new Set<string>());
  const callbacks = useRef(onTerminal);
  callbacks.current = onTerminal;
  const closeSource = useRef<LauncherClosingSource | null>(null);
  if (!closeSource.current) closeSource.current = createLauncherClosingSource();
  const refresh = useCallback(
    () => request.current?.() ?? Promise.resolve(null),
    [],
  );
  const track = useCallback((id: string) => {
    if (id) submitted.current.add(id);
  }, []);
  const acceptStatus = useCallback((status: DownloadStatus) => {
    if (snapshotOwner.current !== binding.current || !status.task_id) return;
    const old = snapshot.current.tasks.find(
      (task) => task.task_id === status.task_id,
    );
    if (!old || (taskIsTerminal(old) && !taskIsTerminal(status))) return;
    const next = {
      ...snapshot.current,
      tasks: snapshot.current.tasks.map((task) =>
        task.task_id === status.task_id ? status : task,
      ),
    };
    snapshot.current = next;
    setView(next);
    if (taskIsTerminal(status) && !terminal.current.has(status.task_id)) {
      terminal.current.add(status.task_id);
      callbacks.current?.(status);
    }
    void request.current?.();
  }, []);
  useEffect(() => {
    let disposed = false,
      closing = false,
      generation = 0,
      demand = 0,
      eventsReady = false,
      readFailed = false;
    let pollTimer: number | undefined, eventTimer: number | undefined;
    const waiters: {
      ticket: number;
      resolve: (view: DownloadTaskView | null) => void;
    }[] = [];
    snapshot.current = emptyTaskView;
    snapshotOwner.current = owner;
    submitted.current.clear();
    observed.current.clear();
    terminal.current.clear();
    setView(emptyTaskView);
    setError("");
    setClosing(false);
    const current = () => !disposed && binding.current === owner && !closing;
    function clearPoll() {
      if (pollTimer !== undefined) window.clearTimeout(pollTimer);
      pollTimer = undefined;
    }
    function clearEvent() {
      if (eventTimer !== undefined) window.clearTimeout(eventTimer);
      eventTimer = undefined;
    }
    function armPoll() {
      clearPoll();
      if (!current()) return;
      const idle = !snapshot.current.tasks.some(taskIsActive);
      // Native events wake new/finished work immediately. Keep the old short
      // fallback when subscription fails, and poll active tasks every second.
      pollTimer = window.setTimeout(
        wakeLatest,
        eventsReady && idle && !readFailed ? 15_000 : 1000,
      );
    }
    function settle(ticket: number, value: DownloadTaskView | null) {
      for (let i = waiters.length - 1; i >= 0; i--)
        if (waiters[i].ticket <= ticket) waiters.splice(i, 1)[0].resolve(value);
    }
    async function run() {
      if (!current() || physical.current) return;
      const token = Symbol(),
        ticket = demand,
        epoch = generation;
      physical.current = token;
      let adopted: DownloadTaskView | null = null;
      try {
        const candidate = await api<unknown>("download_tasks");
        if (!current() || epoch !== generation) return;
        if (!validTaskView(candidate)) throw Error(t("task.invalidSnapshot"));
        const next = adoptTaskView(snapshot.current, candidate);
        snapshot.current = next;
        readFailed = false;
        setView(next);
        setError("");
        adopted = next;
        for (const task of next.tasks) {
          const id = taskIdentity(task);
          if (taskIsActive(task)) observed.current.add(id);
          if (taskIsTerminal(task) && !terminal.current.has(id)) {
            terminal.current.add(id);
            callbacks.current?.(task);
          }
        }
        // Keep retained IDs bounded independently of backend history trimming.
        const retained = new Set(next.tasks.map(taskIdentity));
        for (const ids of [
          submitted.current,
          observed.current,
          terminal.current,
        ])
          if (ids.size > 512)
            for (const id of ids) if (!retained.has(id)) ids.delete(id);
      } catch (cause) {
        if (current() && epoch === generation) {
          readFailed = true;
          setError(String(cause));
        }
      } finally {
        settle(ticket, adopted);
        if (physical.current === token) physical.current = null;
        if (current() && demand > ticket) void run();
        else if (current()) armPoll();
        else if (wake.current !== wakeLatest) wake.current?.();
      }
    }
    function wakeLatest() {
      clearPoll();
      demand++;
      void run();
    }
    function eventWake() {
      if (!current() || eventTimer !== undefined) return;
      // A burst can include several concurrent jobs. The payload is only a
      // hint; one later authoritative read covers the entire burst.
      eventTimer = window.setTimeout(() => {
        eventTimer = undefined;
        wakeLatest();
      }, 50);
    }
    function focusWake() {
      if (document.visibilityState !== "hidden") wakeLatest();
    }
    const refreshLatest = () =>
      new Promise<DownloadTaskView | null>((resolve) => {
        if (!current()) {
          resolve(null);
          return;
        }
        clearPoll();
        clearEvent();
        waiters.push({ ticket: ++demand, resolve });
        void run();
      });
    wake.current = wakeLatest;
    request.current = refreshLatest;
    wakeLatest();
    window.addEventListener("focus", focusWake);
    document.addEventListener("visibilitychange", focusWake);
    let unlistenTask: (() => void) | undefined,
      unlistenClose: (() => void) | undefined;
    if (native) {
      void listen("task_changed", eventWake)
        .then((stop) => {
          if (disposed) stop();
          else {
            unlistenTask = stop;
            eventsReady = true;
            // Events between the first read and subscription may be absent.
            // Demand a read started after listening before using idle fallback.
            wakeLatest();
          }
        })
        .catch(() => {});
      void (subscribeClosing || closeSource.current!)((value) => {
        if (disposed || binding.current !== owner || closing === value) return;
        closing = value;
        generation++;
        setClosing(value);
        if (closing) {
          clearPoll();
          clearEvent();
          settle(Infinity, null);
        } else wakeLatest();
      })
        .then((stop) => {
          if (disposed) stop();
          else unlistenClose = stop;
        })
        .catch(() => {});
    }
    return () => {
      disposed = true;
      generation++;
      clearPoll();
      clearEvent();
      window.removeEventListener("focus", focusWake);
      document.removeEventListener("visibilitychange", focusWake);
      unlistenTask?.();
      unlistenClose?.();
      settle(Infinity, null);
      if (wake.current === wakeLatest) wake.current = null;
      if (request.current === refreshLatest) request.current = null;
    };
  }, [owner]);
  return {
    view: snapshotOwner.current === owner ? view : emptyTaskView,
    error,
    closing: isClosing,
    refresh,
    track,
    acceptStatus,
    snapshot,
    owner,
  };
}
