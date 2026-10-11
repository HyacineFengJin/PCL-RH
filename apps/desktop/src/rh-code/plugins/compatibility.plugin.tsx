import type { BundledPluginEntry } from "../kernel";
import { FolderTree } from "lucide-react";
import { label } from "../locale";
import { LegacyArchive } from "../LegacyArchive";
const entry: BundledPluginEntry = {
  id: "rh.compatibility",
  create: (manifest) => ({
    manifest,
    views: [
      {
        id: "legacy",
        owner: manifest.id,
        label: label("旧版项目", "Previous projects"),
        icon: FolderTree,
        render: LegacyArchive,
      },
    ],
  }),
};
export default entry;
