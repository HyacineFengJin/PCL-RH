import { useEffect, useState } from "react";
import {
  ArrowRightLeft,
  Hammer,
  Package,
  Play,
  Plus,
  Trash2,
  Save,
} from "lucide-react";
import { useCode } from "../controller";
import { label } from "../locale";
import type { Area, Target } from "../types";
export function Targets() {
  const c = useCode(),
    [targets, setTargets] = useState<Target[]>([]);
  useEffect(
    () => setTargets(c.project?.targets ?? []),
    [c.projectId, c.project?.targets],
  );
  return (
    <div className="rc-panel-pad">
      <header className="rc-section-title">
        {label("构建目标", "Build targets")}
      </header>
      {targets.map((target, index) => (
        <section className="rc-target" key={index}>
          <div className="rc-row">
            <input
              aria-label={
                label("Minecraft 版本", "Minecraft version") + " " + (index + 1)
              }
              value={target.minecraft}
              onChange={(e) =>
                setTargets((v) =>
                  v.map((t, i) =>
                    i === index ? { ...t, minecraft: e.target.value } : t,
                  ),
                )
              }
            />
            <button
              className="rc-icon"
              disabled={targets.length <= 1}
              aria-label={label("移除目标", "Remove target")}
              onClick={() => setTargets((v) => v.filter((_, i) => i !== index))}
            >
              <Trash2 size={14} />
            </button>
          </div>
          <div className="rc-row">
            {["Fabric", "NeoForge"].map((loader) => (
              <label key={loader} className="rc-loader">
                <input
                  type="checkbox"
                  checked={target.loaders.includes(loader)}
                  onChange={(e) =>
                    setTargets((v) =>
                      v.map((t, i) =>
                        i === index
                          ? {
                              ...t,
                              loaders: e.target.checked
                                ? [...t.loaders, loader]
                                : t.loaders.filter((l) => l !== loader),
                            }
                          : t,
                      ),
                    )
                  }
                />
                {loader}
              </label>
            ))}
          </div>
        </section>
      ))}
      <div className="rc-row">
        <button
          className="rc-button"
          onClick={() =>
            setTargets((v) => [
              ...v,
              { minecraft: "", loaders: ["Fabric", "NeoForge"] },
            ])
          }
        >
          <Plus size={15} />
          {label("添加版本", "Add version")}
        </button>
        <button
          className="rc-button orange"
          disabled={!c.native || !!c.busy || !targets.length}
          onClick={() =>
            void c
              .write("targets_save", { projectId: c.projectId, targets })
              .catch(() => {})
          }
        >
          <Save size={15} />
          {label("保存", "Save")}
        </button>
      </div>
      <hr />
      <header className="rc-section-title">
        {label("构建与产物", "Build and artifacts")}
      </header>
      <div className="rc-row">
        <button className="rc-button orange" disabled>
          <Hammer size={15} />
          {label("构建", "Build")}
        </button>
        <button className="rc-button" disabled>
          <Package size={15} />
          {label("打包", "Package")}
        </button>
      </div>
      <p className="rc-muted">
        {label(
          "项目构建适配器尚未接入",
          "Project build adapter is not connected yet",
        )}
      </p>
      <header className="rc-section-title">
        {label("游戏测试", "Game test")}
      </header>
      <button className="rc-button" disabled>
        <Play size={15} />
        {label("启动测试", "Launch test")}
      </button>
    </div>
  );
}
export function Porting({ area }: { area: Area }) {
  const c = useCode(),
    [version, setVersion] = useState("");
  async function discuss() {
    if (!c.project) return;
    let conversation: string | undefined = c.project.conversations[0]?.id;
    if (!conversation) conversation = await c.newConversation();
    if (!conversation) return;
    c.setDraft(
      conversation,
      label(
        "分析这个项目迁移到 Minecraft ",
        "Analyze migration of this project to Minecraft ",
      ) +
        version +
        label(
          " 的 API、元数据与资源差异，先规划，再提出补丁。保留原玩法与手写代码。",
          ". Check API, metadata and resource differences; plan before proposing patches. Preserve gameplay and handwritten code.",
        ),
    );
    c.openConversation(conversation);
  }
  return (
    <div className="rc-panel-pad">
      <header className="rc-section-title">
        <ArrowRightLeft size={16} />
        {label("版本迁移", "Version migration")}
      </header>
      <label className="rc-field">
        {label("目标 Minecraft 版本", "Target Minecraft version")}
        <input
          value={version}
          onChange={(e) => setVersion(e.target.value)}
          placeholder="1.21.4"
        />
      </label>
      <button
        className="rc-button orange"
        disabled={!version.trim() || !c.native || !!c.busy}
        onClick={() => void discuss().catch(() => {})}
      >
        {label("在项目中讨论", "Discuss in this project")}
      </button>
    </div>
  );
}
