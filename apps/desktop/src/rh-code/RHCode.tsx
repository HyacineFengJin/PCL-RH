/** Product shell: native project service and the Cordis registry outlive every
 * split area. The launcher shell neither knows nor mounts individual views. */
import { useEffect, useState } from "react";
import { ArrowLeft, Settings2, LayoutDashboard, X } from "lucide-react";
import { CeSelect } from "../CeSelect";
import { ExperimentalAi } from "../ExperimentalAi";
import type { PorterIntake } from "../experimentalPorterTypes";
import type { Api } from "../types";
import type { ExperimentalNavigate } from "../experimentalTypes";
import { CodeContext, useController, type Controller } from "./controller";
import type { Kernel } from "./kernel";
import { LegacyArchive } from "./LegacyArchive";
import { Projects } from "./Projects";
import { Dialog } from "./Dialog";
import { Plugins } from "./Plugins";
import { Workspace } from "./Workspace";
import { Layouts } from "./Layouts";
import { label } from "./locale";
import { CodeSurfaceVisibility } from "./surface";
import "./rh-code.css";
export function RHCode({
  api,
  native,
  visible,
  section,
  onNavigate,
  porterIntake,
  onPorterIntakeConsumed,
  kernel,
  onController,
  onConfiguration,
}: {
  api: Api;
  native: boolean;
  visible: boolean;
  section: "code" | "plugins";
  onNavigate: ExperimentalNavigate;
  porterIntake?: PorterIntake | null;
  onPorterIntakeConsumed?: () => void;
  kernel: Kernel;
  onController: (controller: Controller | null) => void;
  onConfiguration: (disabled: string[]) => void;
}) {
  const [version, refresh] = useState(0),
    [settings, setSettings] = useState(false),
    [layoutMenu, setLayoutMenu] = useState(false),
    [archive, setArchive] = useState(false);
  const c = useController(
    api,
    native,
    visible && (section === "code" || settings),
  );
  useEffect(() => {
    onController(c);
  });
  useEffect(() => () => onController(null), [onController]);
  const pluginConfiguration = c.snapshot
    ? JSON.stringify([...c.snapshot.disabledPlugins].sort())
    : null;
  useEffect(() => {
    if (porterIntake) setArchive(true);
  }, [porterIntake]);
  useEffect(() => kernel.subscribe(() => refresh((n) => n + 1)), [kernel]);
  useEffect(() => {
    if (pluginConfiguration !== null)
      onConfiguration(JSON.parse(pluginConfiguration) as string[]);
  }, [onConfiguration, pluginConfiguration]);
  const p = c.project;
  void version;
  return (
    <CodeContext.Provider
      value={{
        ...c,
        openAiSettings: () => setSettings(true),
        run: (...args) =>
          kernel.invoke<ReturnType<typeof c.run>>("agent.run", ...args),
        // Admission belongs to the launch plugin. The controller owns admitted
        // jobs until their children drain, so plugin retirement cannot revoke
        // cancellation while a project task is still running.
        stop: c.stop,
      }}
    >
      <CodeSurfaceVisibility.Provider value={visible}>
        <div
          data-surface={
            section === "plugins" ? "plugins" : p ? "workspace" : "projects"
          }
          className={
            "rh-code " + (section === "plugins" ? "rc-plugin-space" : "")
          }
        >
          {c.error && (
            <div className="rc-error" role="alert">
              <span>{c.error}</span>
              <button
                className="rc-button"
                onClick={() =>
                  void c.read().catch((e) => c.setError(String(e)))
                }
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
              onClose={() => {
                setSettings(false);
                void c.refreshEngine().catch((e) => c.setError(String(e)));
              }}
              className="rc-settings-dialog"
            >
              <div className="rc-settings-scroll">
                <ExperimentalAi api={api} native={native} />
                <section className="rc-settings-info">
                  <span>RH Code</span>
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
      </CodeSurfaceVisibility.Provider>
    </CodeContext.Provider>
  );
}
