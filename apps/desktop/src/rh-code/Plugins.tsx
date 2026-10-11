import { useState } from "react";
import { Puzzle, Blocks, ShieldCheck, Store, RotateCcw } from "lucide-react";
import { useCode } from "./controller";
import { label } from "./locale";
import { ExperimentalExtensions } from "../ExperimentalExtensions";
import type { ExperimentalNavigate } from "../experimentalTypes";
import type { Kernel } from "./kernel";
import type { PluginScope, PluginStatus } from "./types";
const scopeNames: Record<PluginScope, [string, string]> = {
  launcher: ["启动器", "Launcher"],
  "rh-code": ["RH Code", "RH Code"],
  both: ["启动器与 RH Code", "Launcher and RH Code"],
};
const statusNames: Record<PluginStatus["state"], [string, string]> = {
  active: ["已启用", "Active"],
  dormant: ["休眠", "Dormant"],
  disabled: ["已停用", "Disabled"],
  blocked: ["依赖未就绪", "Dependency unavailable"],
  failed: ["加载失败", "Failed"],
  stopped: ["已停止", "Stopped"],
};
const names: Record<string, [string, string]> = {
  "rh.compatibility": ["旧版项目兼容", "Previous project compatibility"],
  "rh.project": ["项目服务", "Project service"],
  "rh.conversation": ["对话工作区", "Conversations"],
  "rh.agent": ["Agent 调度", "Agent scheduler"],
  "rh.files": ["文件图谱", "Project files"],
  "rh.graph": ["内容图谱", "Content graph"],
  "rh.source": ["源码编辑", "Source editor"],
  "rh.targets": ["构建目标", "Build targets"],
  "rh.changes": ["改动审阅", "Change review"],
  "rh.tasks": ["任务与输出", "Tasks and output"],
  "rh.porting": ["版本迁移", "Version migration"],
  "rh.settings": ["工作区设置", "Workspace settings"],
  "rh.overview": ["项目概览", "Project overview"],
};
export function Plugins({
  kernel,
  onNavigate,
}: {
  kernel: Kernel;
  onNavigate: ExperimentalNavigate;
}) {
  const c = useCode(),
    [page, setPage] = useState("builtin"),
    [scope, setScope] = useState<"all" | PluginScope>("all");
  const visible = c.snapshot?.plugins.filter(
    (plugin) => scope === "all" || (plugin.scope ?? "rh-code") === scope,
  );
  return (
    <div className="rc-plugins">
      <header>
        <h1>
          <Puzzle size={24} />
          {label("插件", "Plugins")}
        </h1>
        <div className="rc-segments">
          {[
            ["builtin", label("内置模块", "Built-in modules"), Blocks],
            ["community", label("社区插件", "Community plugins"), ShieldCheck],
            ["store", label("商店", "Store"), Store],
          ].map(([id, text, Icon]) => {
            const I = Icon as typeof Blocks;
            return (
              <button
                key={id as string}
                aria-pressed={page === id}
                onClick={() => setPage(id as string)}
              >
                <I size={15} />
                {text as string}
              </button>
            );
          })}
        </div>
      </header>
      {page === "builtin" ? (
        <>
          <div className="rc-plugin-scope-filter">
            <span>{label("运行范围", "Runs in")}</span>
            <div
              className="rc-segments"
              role="group"
              aria-label={label("筛选运行范围", "Filter plugin scope")}
            >
              {(["all", "launcher", "rh-code", "both"] as const).map(
                (value) => (
                  <button
                    key={value}
                    aria-pressed={scope === value}
                    onClick={() => setScope(value)}
                  >
                    {value === "all"
                      ? label("全部", "All")
                      : label(...scopeNames[value])}
                  </button>
                ),
              )}
            </div>
          </div>
          <div className="rc-plugin-list">
            {visible?.map((p) => {
              const enabled = !c.snapshot!.disabledPlugins.includes(p.id);
              const status = kernel.statuses.get(p.id),
                plugin = kernel.plugins.find((q) => q.manifest.id === p.id);
              const Icon = plugin?.views?.[0]?.icon ?? Blocks;
              const stateLabel = label(
                ...(status
                  ? statusNames[status.state]
                  : !enabled
                    ? statusNames.disabled
                    : ["正在加载", "Loading"]),
              );
              const declaredScope = p.scope ?? "rh-code";
              const dependents = c.snapshot!.plugins.filter(
                (q) =>
                  q.dependencies.includes(p.id) &&
                  !c.snapshot!.disabledPlugins.includes(q.id),
              );
              const blocked =
                p.required ||
                (enabled
                  ? dependents.length > 0
                  : p.dependencies.some((d) =>
                      c.snapshot!.disabledPlugins.includes(d),
                    ));
              return (
                <article className="rc-plugin" key={p.id}>
                  <div className="rc-plugin-icon">
                    <Icon size={22} />
                  </div>
                  <div>
                    <h2>
                      {label(...(names[p.id] ?? [p.name, p.name]))}
                      <small>{p.version}</small>
                    </h2>
                    <div className="rc-plugin-tags">
                      <span
                        className="rc-plugin-scope"
                        data-scope={p.scope ?? "rh-code"}
                      >
                        {label(
                          ...(scopeNames[declaredScope] ?? [
                            declaredScope,
                            declaredScope,
                          ]),
                        )}
                      </span>
                      <span
                        className={
                          "rc-plugin-status " + (status?.state ?? "loading")
                        }
                      >
                        {stateLabel}
                      </span>
                    </div>
                    <details>
                      <summary>{label("模块详情", "Module details")}</summary>
                      <p>
                        <code>{p.id}</code>
                      </p>
                      {p.dependencies.length > 0 && (
                        <p>
                          {label("依赖：", "Depends on: ")}
                          {p.dependencies
                            .map((id) => label(...(names[id] ?? [id, id])))
                            .join(" · ")}
                        </p>
                      )}
                      <ul className="rc-plugin-contributions">
                        {p.views.map((id) => {
                          const contribution = plugin?.views?.find(
                            (view) => view.id === id,
                          );
                          const ViewIcon = contribution?.icon ?? Blocks;
                          return (
                            <li key={id}>
                              <ViewIcon size={14} />
                              {contribution && (
                                <span>{contribution.label}</span>
                              )}
                              <code>{id}</code>
                            </li>
                          );
                        })}
                      </ul>
                      {(p.commands?.length ?? 0) > 0 && (
                        <p>
                          {label("命令：", "Commands: ")}
                          {p.commands!.join(" · ")}
                        </p>
                      )}
                      {(p.layouts?.length ?? 0) > 0 && (
                        <p>
                          {label("布局：", "Layouts: ")}
                          {p
                            .layouts!.map(
                              (id) =>
                                plugin?.layouts?.find(
                                  (layout) => layout.id === id,
                                )?.label ?? id,
                            )
                            .join(" · ")}
                        </p>
                      )}
                      <p>{p.capabilities.join(" · ")}</p>
                    </details>
                    {status?.state === "failed" && (
                      <div className="rc-plugin-failure">
                        <p className="rc-error-text">{status.error}</p>
                        <button
                          className="rc-button"
                          onClick={() =>
                            void kernel
                              .retry(p.id)
                              .catch((e) => c.setError(String(e)))
                          }
                        >
                          <RotateCcw size={14} />
                          {label("重新加载", "Reload")}
                        </button>
                      </div>
                    )}
                  </div>
                  <button
                    className="rc-toggle"
                    role="switch"
                    aria-checked={enabled}
                    aria-label={
                      label("启用模块", "Enable module") + " · " + p.id
                    }
                    disabled={!c.native || !!c.busy || !!blocked}
                    title={
                      p.required
                        ? label("核心模块", "Core module")
                        : blocked
                          ? label(
                              "请先调整依赖模块",
                              "Adjust dependent modules first",
                            )
                          : undefined
                    }
                    onClick={() =>
                      void c
                        .write("plugin_set", { id: p.id, enabled: !enabled })
                        .catch(() => {})
                    }
                  >
                    <span />
                  </button>
                </article>
              );
            })}
          </div>
          {c.snapshot && visible?.length === 0 && (
            <p className="rc-plugin-scope-empty">
              {label(
                "此运行范围暂无内置模块",
                "No built-in modules in this scope",
              )}
            </p>
          )}
        </>
      ) : page === "community" ? (
        <div className="rc-plugin-community">
          <p className="rc-plugin-community-note">
            {label(
              "社区插件目前支持声明型扩展，暂不支持加载可执行插件代码。",
              "Community plugins currently support declarative extensions. Executable plugin code is not supported yet.",
            )}
          </p>
          <ExperimentalExtensions
            api={c.api}
            native={c.native}
            onNavigate={onNavigate}
          />
        </div>
      ) : (
        <div className="rc-chat-empty">
          <Store size={33} />
          <h2>{label("插件商店", "Plugin store")}</h2>
          <p>{label("暂未开放", "Not available yet")}</p>
        </div>
      )}
    </div>
  );
}
