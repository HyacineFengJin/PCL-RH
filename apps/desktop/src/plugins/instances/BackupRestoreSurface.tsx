import { useEffect, useMemo, useRef, useState } from "react";
import { createTranslator } from "../../i18n";
import type { LauncherSurfaceProps } from "../../productPlugins/surfaceTypes";
import type {
  BackupSelection,
  BackupRestorePreview,
} from "../../productPlugins/backupRestore";
import "./snapshots.css";

export function BackupRestoreSurface({ host, lifetime }: LauncherSurfaceProps) {
  const port = host.instanceSnapshots;
  const connection = port?.restore;
  const context = useMemo(() => ({}), [connection, port?.available, lifetime]);
  const [draft, setDraft] = useState<{
    context: object;
    selection?: BackupSelection;
    name: string;
  }>({ context, name: "" });
  const form: typeof draft =
    draft.context === context ? draft : { context, name: "" };
  const owner = useMemo(() => ({}), [context, form.selection, form.name]);
  const current = useRef({ owner, live: true });
  current.current.owner = owner;
  useEffect(() => {
    current.current.live = true;
    return () => {
      current.current.live = false;
    };
  }, []);
  const [state, setState] = useState<{
    owner: object;
    busy?: boolean;
    preview?: BackupRestorePreview;
    error?: string;
    cancelled?: boolean;
  }>({ owner });
  const working = useRef<object | null>(null);
  if (!port || !connection) return null;
  const visible: typeof state = state.owner === owner ? state : { owner };
  const active = () =>
    current.current.live &&
    current.current.owner === owner &&
    port.available &&
    lifetime.isActive();
  const tr = createTranslator(port.localization);
  async function perform(kind: "pick" | "prepare" | "start") {
    if (
      !active() ||
      working.current === owner ||
      (kind === "prepare" && !form.selection) ||
      (kind === "start" && !visible.preview)
    )
      return;
    working.current = owner;
    setState({ owner, busy: true, preview: visible.preview });
    try {
      if (kind === "pick") {
        const selection = await connection!.pick();
        if (active()) {
          if (selection) {
            setDraft({ context, selection, name: selection.suggestedName });
            setState({ owner });
          } else setState({ owner, cancelled: true, preview: visible.preview });
        }
      } else if (kind === "prepare") {
        const preview = await connection!.prepare(form.selection!, form.name);
        if (active()) setState({ owner, preview });
      } else {
        await connection!.start(visible.preview!, active);
        if (active()) setState({ owner });
      }
    } catch (error) {
      if (active()) setState({ owner, error: String(error) });
    } finally {
      if (working.current === owner) working.current = null;
    }
  }
  return (
    <section
      className="ce-card ce-instance-snapshots"
      aria-busy={!!visible.busy}
    >
      <h2 className="ce-card-title">{tr.t("backupRestore.title")}</h2>
      <p className="muted">{tr.t("backupRestore.hint")}</p>
      {form.selection && (
        <>
          <p className="muted">{form.selection.filename}</p>
          <label className="ce-row">
            <span>{tr.t("copy.name")}</span>
            <input
              className="ce-field"
              value={form.name}
              disabled={!port.available || visible.busy}
              onChange={(event) => {
                if (active() && !visible.busy)
                  setDraft({ ...form, name: event.target.value });
              }}
            />
          </label>
        </>
      )}
      {visible.error && (
        <p className="error-text" role="alert">
          {visible.error}
        </p>
      )}
      {visible.cancelled && (
        <p role="status">{tr.t("backupRestore.cancelled")}</p>
      )}
      {visible.preview && (
        <div role="status">
          <p>
            {visible.preview.packName} · {visible.preview.packVersion} ·
            Minecraft {visible.preview.minecraft}
          </p>
          <p>
            {tr.t("snapshot.preview", {
              count: tr.formatNumber(visible.preview.fileCount),
              size: tr.formatNumber(visible.preview.bytes / 1024 ** 2, {
                maximumFractionDigits: 1,
              }),
            })}
          </p>
          {visible.preview.warnings.map((warning, index) => (
            <p className="muted" key={index}>
              {warning}
            </p>
          ))}
        </div>
      )}
      <div className="ce-actions">
        <button
          className="ce-button"
          disabled={!port.available || visible.busy}
          onClick={() => void perform("pick")}
        >
          {tr.t("backupRestore.pick")}
        </button>
        {form.selection && (
          <button
            className="ce-button"
            disabled={!port.available || visible.busy || !form.name.trim()}
            onClick={() => void perform("prepare")}
          >
            {tr.t(visible.busy ? "snapshot.working" : "backupRestore.inspect")}
          </button>
        )}
        {visible.preview && (
          <button
            className="ce-button primary"
            disabled={!port.available || visible.busy}
            onClick={() => void perform("start")}
          >
            {tr.t("backupRestore.start")}
          </button>
        )}
      </div>
    </section>
  );
}
