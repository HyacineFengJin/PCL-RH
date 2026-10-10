import { useState } from "react";
import { Puzzle, Blocks, ShieldCheck, Store } from "lucide-react";
import { useCode } from "./controller";
import { label } from "./locale";
import { ExperimentalExtensions } from "../ExperimentalExtensions";
import type { ExperimentalNavigate } from "../experimentalTypes";
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
};
export function Plugins({ onNavigate }: { onNavigate: ExperimentalNavigate }) {
  const c = useCode(),
    [page, setPage] = useState("builtin");
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
        <div className="rc-plugin-list">
          {c.snapshot?.plugins.map((p) => {
            const enabled = !c.snapshot!.disabledPlugins.includes(p.id);
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
                  <Blocks size={22} />
                </div>
                <div>
                  <h2>
                    {label(...(names[p.id] ?? [p.name, p.name]))}
                    <small>{p.version}</small>
                  </h2>
                  <code>{p.id}</code>
                  {p.dependencies.length > 0 && (
                    <p>
                      {label("依赖：", "Depends on: ")}
                      {p.dependencies
                        .map((id) => label(...(names[id] ?? [id, id])))
                        .join(" · ")}
                    </p>
                  )}
                  <details>
                    <summary>
                      {label("模块能力", "Module capabilities")}
                    </summary>
                    <p>{p.capabilities.join(" · ")}</p>
                  </details>
                </div>
                <button
                  className="rc-toggle"
                  role="switch"
                  aria-checked={enabled}
                  aria-label={label("启用模块", "Enable module") + " · " + p.id}
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
      ) : page === "community" ? (
        <ExperimentalExtensions
          api={c.api}
          native={c.native}
          onNavigate={onNavigate}
        />
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
