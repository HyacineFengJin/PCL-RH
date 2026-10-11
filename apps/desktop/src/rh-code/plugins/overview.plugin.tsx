/** This small first-party component depends only on the public view host. It
 * demonstrates per-area settings without importing the workspace controller. */
import { LayoutDashboard } from "lucide-react";
import type { BundledPluginEntry } from "../kernel";
import type { ViewProps } from "../types";
import { label } from "../locale";
export function Overview({ api }: ViewProps) {
  const project = api.project;
  if (!project)
    return (
      <p className="rc-muted">{label("请选择项目", "Choose a project")}</p>
    );
  return (
    <div className="rc-panel-pad">
      <header className="rc-section-title">{project.name}</header>
      {api.state.showFolder !== false && (
        <p className="rc-muted" style={{ overflowWrap: "anywhere" }}>
          {project.folder}
        </p>
      )}
      <p>
        {label("构建目标", "Build targets")}: {project.targets.length}
      </p>
      <p>
        {label("项目对话", "Project conversations")}:{" "}
        {project.conversations.length}
      </p>
    </div>
  );
}
export function OverviewSettings({ api }: ViewProps) {
  return (
    <label className="rc-loader">
      <input
        type="checkbox"
        checked={api.state.showFolder !== false}
        onChange={(event) =>
          api.updateState({ ...api.state, showFolder: event.target.checked })
        }
      />
      {label("显示项目目录", "Show project folder")}
    </label>
  );
}
const entry: BundledPluginEntry = {
  id: "rh.overview",
  create: (manifest) => ({
    manifest,
    views: [
      {
        id: "rh.overview.summary",
        owner: manifest.id,
        label: label("项目概览", "Project overview"),
        icon: LayoutDashboard,
        render: Overview,
        settings: OverviewSettings,
      },
    ],
  }),
};
export default entry;
