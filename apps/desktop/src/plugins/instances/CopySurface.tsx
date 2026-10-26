import { useEffect, useMemo, useRef, useState } from "react";
import { createTranslator } from "../../i18n";
import type { LauncherSurfaceProps } from "../../productPlugins/surfaceTypes";
import type {
  CopyOptions,
  CopyPreview,
} from "../../productPlugins/instanceCopy";
import "./snapshots.css";

/** Form and preview belong to one plugin activation and one bound target. */
export function CopySurface({ host, lifetime }: LauncherSurfaceProps) {
  const port = host.instanceCopy;
  const target = useMemo(
    () => ({}),
    [port?.connection, port?.available, lifetime],
  );
  const [form, setForm] = useState<{ target: object; options: CopyOptions }>({
    target,
    options: {
      name: port?.suggestedName ?? "",
      saves: false,
      screenshots: false,
    },
  });
  const options =
    form.target === target
      ? form.options
      : { name: port?.suggestedName ?? "", saves: false, screenshots: false };
  const owner = useMemo(
    () => ({}),
    [target, options.name, options.saves, options.screenshots],
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
    preview?: CopyPreview;
    error?: string;
    busy?: boolean;
  }>({ owner });
  const working = useRef<object | null>(null);
  if (!port) return null;
  const tr = createTranslator(port.localization);
  const visible: typeof state = state.owner === owner ? state : { owner };
  const active = () =>
    current.current.live &&
    current.current.owner === owner &&
    port.available &&
    lifetime.isActive();
  const pending = visible.busy === true;
  const change = (values: Partial<CopyOptions>) => {
    if (!active() || pending) return;
    setForm({ target, options: { ...options, ...values } });
  };
  async function perform(start: boolean) {
    if (!active() || working.current === owner || (start && !visible.preview))
      return;
    working.current = owner;
    setState({ owner, preview: visible.preview, busy: true });
    try {
      if (start) {
        await port!.connection.start(visible.preview!, active);
        if (active()) setState({ owner });
      } else {
        const preview = await port!.connection.prepare(options);
        if (active()) setState({ owner, preview });
      }
    } catch (error) {
      if (active()) setState({ owner, error: String(error) });
    } finally {
      if (working.current === owner) working.current = null;
    }
  }
  return (
    <section className="ce-card ce-instance-snapshots" aria-busy={pending}>
      <h2 className="ce-card-title">{tr.t("copy.title")}</h2>
      <label className="ce-row">
        <span>{tr.t("copy.name")}</span>
        <input
          className="ce-field"
          value={options.name}
          disabled={!port.available || pending}
          onChange={(e) => change({ name: e.target.value })}
        />
      </label>
      <div className="ce-actions">
        <label>
          <input
            type="checkbox"
            checked={options.saves}
            disabled={!port.available || pending}
            onChange={(e) => change({ saves: e.target.checked })}
          />{" "}
          {tr.t("copy.saves")}
        </label>
        <label>
          <input
            type="checkbox"
            checked={options.screenshots}
            disabled={!port.available || pending}
            onChange={(e) => change({ screenshots: e.target.checked })}
          />{" "}
          {tr.t("copy.screenshots")}
        </label>
      </div>
      <p className="muted">{tr.t("copy.hint")}</p>
      {visible.error && (
        <p className="error-text" role="alert">
          {visible.error}
        </p>
      )}
      {visible.preview && (
        <p role="status">
          {tr.t("snapshot.preview", {
            count: tr.formatNumber(visible.preview.fileCount),
            size: tr.formatNumber(visible.preview.bytes / 1024 ** 2, {
              maximumFractionDigits: 1,
            }),
          })}
        </p>
      )}
      <div className="ce-actions">
        <button
          className="ce-button"
          disabled={!port.available || pending || !options.name.trim()}
          onClick={() => void perform(false)}
        >
          {tr.t(pending ? "snapshot.working" : "copy.inspect")}
        </button>
        {visible.preview && (
          <button
            className="ce-button primary"
            disabled={!port.available || pending}
            onClick={() => void perform(true)}
          >
            {tr.t("copy.start")}
          </button>
        )}
      </div>
    </section>
  );
}
