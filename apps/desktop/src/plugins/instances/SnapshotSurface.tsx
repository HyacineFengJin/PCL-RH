/** Presets, preview and asynchronous form state belong to this activation.
 * The native export service owns path selection, consistency checks and writes.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { CeSelect } from "../../CeSelect";
import { createTranslator } from "../../i18n";
import type { LauncherSurfaceProps } from "../../productPlugins/surfaceTypes";
import type {
  SnapshotMode,
  SnapshotPreview,
} from "../../productPlugins/instanceSnapshots";
import "./snapshots.css";

export function SnapshotSurface({ host, lifetime }: LauncherSurfaceProps) {
  const port = host.instanceSnapshots;
  const [mode, setMode] = useState<SnapshotMode>("backup");
  const owner = useMemo(
    () => ({}),
    [port?.connection, port?.available, lifetime, mode],
  );
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
    preview?: SnapshotPreview;
    error?: string;
    cancelled?: boolean;
    busy?: boolean;
  }>({ owner });
  const working = useRef<{ owner: object } | null>(null);
  if (!port) return null;
  const tr = createTranslator(port.localization);
  const visible: typeof state = state.owner === owner ? state : { owner };
  const pending = working.current?.owner === owner;
  const active = () =>
    current.current.live &&
    current.current.owner === owner &&
    port.available &&
    lifetime.isActive();
  async function perform(start: boolean) {
    if (
      !active() ||
      working.current?.owner === owner ||
      (start && !visible.preview)
    )
      return;
    const operation = { owner };
    working.current = operation;
    setState({ owner, preview: visible.preview, busy: true });
    try {
      if (start) {
        const task = await port!.connection.start(visible.preview!, active);
        if (active()) setState({ owner, cancelled: !task });
      } else {
        const preview = await port!.connection.prepare(mode);
        if (active()) setState({ owner, preview });
      }
    } catch (error) {
      if (active()) setState({ owner, error: String(error) });
    } finally {
      if (working.current === operation) working.current = null;
    }
  }
  return (
    <section
      className="ce-card ce-instance-snapshots"
      aria-busy={!!visible.busy}
    >
      <h2 className="ce-card-title">{tr.t("snapshot.title")}</h2>
      <label className="ce-row">
        <span>{tr.t("snapshot.preset")}</span>
        <CeSelect
          className="ce-field"
          value={mode}
          disabled={!port.available || pending}
          onChange={(event) => {
            if (!active() || working.current?.owner === owner) return;
            setMode(event.target.value as SnapshotMode);
            setState({ owner });
          }}
        >
          <option value="backup">{tr.t("snapshot.backup")}</option>
          <option value="share">{tr.t("snapshot.share")}</option>
        </CeSelect>
      </label>
      <p className="muted">
        {tr.t(mode === "backup" ? "snapshot.backupHint" : "snapshot.shareHint")}
      </p>
      {visible.error && (
        <p className="error-text" role="alert">
          {visible.error}
        </p>
      )}
      {visible.cancelled && (
        <p role="status">{tr.t("export.locationCancelled")}</p>
      )}
      {visible.preview && (
        <div role="status">
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
          disabled={!port.available || pending}
          onClick={() => void perform(false)}
        >
          {tr.t(visible.busy ? "snapshot.working" : "snapshot.inspect")}
        </button>
        {visible.preview && (
          <button
            className="ce-button primary"
            disabled={!port.available || pending}
            onClick={() => void perform(true)}
          >
            {tr.t("snapshot.save")}
          </button>
        )}
      </div>
    </section>
  );
}
