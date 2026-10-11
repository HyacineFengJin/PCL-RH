/** Product shell: native project service and the Cordis registry outlive every
 * split area. The launcher shell neither knows nor mounts individual views. */
import { useEffect, useMemo, useState, useRef } from "react";
import { ArrowLeft, Settings2, LayoutDashboard, X, Puzzle } from "lucide-react";
import { CeSelect } from "../CeSelect";
import { ExperimentalAi } from "../ExperimentalAi";
import type { PorterIntake } from "../experimentalPorterTypes";
import type { Api } from "../types";
import type { ExperimentalNavigate } from "../experimentalTypes";
import { CodeContext, useController } from "./controller";
import { Kernel } from "./kernel";
import { productPlugins } from "./plugins";
import { LegacyArchive } from "./LegacyArchive";
import { Projects } from "./Projects";
import { Dialog } from "./Dialog";
import { Plugins } from "./Plugins";
import { Workspace } from "./Workspace";
import { Layouts } from "./Layouts";
import { label } from "./locale";
import "./rh-code.css";
export function RHCode({
  api,
  native,
  section,
  onNavigate,
  porterIntake,
  onPorterIntakeConsumed,
}: {
  api: Api;
  native: boolean;
  section: "code" | "plugins";
  onNavigate: ExperimentalNavigate;
  porterIntake?: PorterIntake | null;
  onPorterIntakeConsumed?: () => void;
}) {
  const c = useController(api, native),
    live = useRef(c);
  live.current = c;
  const pluginConfiguration = c.snapshot
    ? JSON.stringify([...c.snapshot.disabledPlugins].sort())
    : null;
  const kernel = useMemo(() => {
      const k: Kernel = new Kernel(
        productPlugins(
          {
            run: (...args: Parameters<typeof c.run>) =>
              live.current.run(...args),
            stop: (id: string) => live.current.stop(id),
          },
          () => k,
        ),
      );
      return k;
    }, []),
    [version, refresh] = useState(0),
    [settings, setSettings] = useState(false),
    [layoutMenu, setLayoutMenu] = useState(false),
    [archive, setArchive] = useState(false);
  useEffect(() => {
    if (porterIntake) setArchive(true);
  }, [porterIntake]);
  useEffect(() => kernel.subscribe(() => refresh((n) => n + 1)), [kernel]);
  useEffect(() => {
    if (pluginConfiguration !== null)
      void kernel
        .configure(JSON.parse(pluginConfiguration) as string[])
        .catch((e) => c.setError(String(e)));
  }, [kernel, pluginConfiguration]);
  useEffect(
    () => () => {
      void kernel.dispose();
    },
    [kernel],
  );
  const p = c.project;
  void version;
  return (
    <CodeContext.Provider
      value={{
        ...c,
        run: (...args) =>
          kernel.invoke<ReturnType<typeof c.run>>("agent.run", ...args),
        stop: (id) =>
          kernel.invoke<ReturnType<typeof c.stop>>("agent.stop", id),
      }}
    >
      <div
        className={
          "rh-code " + (section === "plugins" ? "rc-plugin-space" : "")
        }
      >
        {c.error && (
          <div className="rc-error" role="alert">
            <span>{c.error}</span>
            <button
              className="rc-button"
              onClick={() => void c.read().catch((e) => c.setError(String(e)))}
            >
              {label("刷新", "Refresh")}
            </button>
            <button
              className="rc-icon"
              aria-label={label("关闭提示", "Dismiss")}
              onClick={() => c.setError("")}
            >
              <X size={15} />
            </button>
          </div>
        )}
        {section === "plugins" ? (
          <Plugins kernel={kernel} onNavigate={onNavigate} />
        ) : p ? (
          <>
            <header className="rc-workbar">
              <button
                className="rc-icon"
                aria-label={label("项目列表", "Project list")}
                onClick={c.home}
              >
                <ArrowLeft size={17} />
              </button>
              <CeSelect
                className="rc-project-switch"
                aria-label={label("切换项目", "Switch project")}
                value={p.id}
                onChange={(e) => {
                  const next = c.snapshot?.projects.find(
                    (p) => p.id === e.target.value,
                  );
                  if (next) c.choose(next);
                }}
              >
                {c.snapshot?.projects.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </CeSelect>
              <span className="rc-project-path" title={p.folder}>
                {p.folder}
              </span>
              <span className="rc-spacer" />
              <div className="rc-layout-menu">
                <button
                  className="rc-button"
                  onClick={() => setLayoutMenu(true)}
                  aria-expanded={layoutMenu}
                >
                  <LayoutDashboard size={15} />
                  {label("布局", "Layout")}
                </button>
              </div>
            </header>
            <Workspace kernel={kernel} />
            {layoutMenu && (
              <Layouts kernel={kernel} onClose={() => setLayoutMenu(false)} />
            )}
            <footer className="rc-status">
              <span className="rc-status-project">
                {p.targets.map((t) => t.minecraft).join(" / ")} <b>·</b>{" "}
                {p.targets[0]?.loaders.join(" + ")}
              </span>
              <span className="rc-spacer" />
              <span>
                {p.conversations.filter((t) => t.activeJob).length}{" "}
                {label("个活动任务", "active tasks")}
              </span>
            </footer>
          </>
        ) : (
          <Projects onArchive={() => setArchive(true)} />
        )}
        <button
          className="rc-settings-orb"
          aria-label={label("RH Code 设置", "RH Code settings")}
          onClick={() => setSettings(true)}
        >
          <Settings2 size={20} />
        </button>
        {archive && (
          <Dialog
            title={label("旧版项目", "Previous projects")}
            onClose={() => setArchive(false)}
            className="rc-settings-dialog rc-archive-dialog"
          >
            <LegacyArchive
              intake={porterIntake}
              onConsumed={onPorterIntakeConsumed}
            />
          </Dialog>
        )}
        {settings && (
          <Dialog
            title={label("RH Code 设置", "RH Code settings")}
            onClose={() => setSettings(false)}
            className="rc-settings-dialog"
          >
            <div className="rc-settings-scroll">
              <ExperimentalAi api={api} native={native} />
              <section className="rc-settings-info">
                <h3>{label("工作区", "Workspace")}</h3>
                <p>
                  {label(
                    "区域布局按项目保存；关闭区域保留对话与源码。",
                    "Area layouts are saved per project. Closing an area retains conversations and source.",
                  )}
                </p>
                <a
                  href="https://github.com/deepseek-ai/deepseek-harness"
                  target="_blank"
                  rel="noreferrer"
                >
                  DeepSeek Harness ↗
                </a>
              </section>
            </div>
          </Dialog>
        )}
      </div>
    </CodeContext.Provider>
  );
}
