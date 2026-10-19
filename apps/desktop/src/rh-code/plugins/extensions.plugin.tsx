/** External packages contribute data to this trusted view. They never receive
 * the controller, project files, native IPC or an executable render callback. */
import { Puzzle } from "lucide-react";
import { ExperimentalCards } from "../../ExperimentalExtensions";
import type { ExperimentalNavigate } from "../../experimentalTypes";
import { useCode } from "../controller";
import type { BundledPluginEntry } from "../kernel";
import { label } from "../locale";
import { useCodeSurfaceVisible } from "../surface";

export function CommunityCards({
  onNavigate,
}: {
  onNavigate: ExperimentalNavigate;
}) {
  const c = useCode(),
    visible = useCodeSurfaceVisible();
  return (
    <div className="rc-panel-pad rc-community-cards">
      <ExperimentalCards
        api={c.api}
        native={c.native && visible}
        slot="rh-code.cards"
        onNavigate={onNavigate}
        emptyText={label(
          "暂无启用的工作区卡片，可通过顶部橙色入口导入插件。",
          "No workspace cards are enabled. Import a plugin from the orange entry above.",
        )}
      />
    </div>
  );
}

const entry: BundledPluginEntry = {
  id: "rh.extensions",
  create: (manifest, runtime) => ({
    manifest,
    views: [
      {
        id: "rh.extensions.cards",
        owner: manifest.id,
        label: label("插件卡片", "Plugin cards"),
        icon: Puzzle,
        tone: "orange",
        render: () => (
          <CommunityCards onNavigate={runtime.navigate ?? (() => {})} />
        ),
      },
    ],
  }),
};
export default entry;
