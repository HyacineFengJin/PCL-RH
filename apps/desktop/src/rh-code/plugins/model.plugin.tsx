import type { BundledPluginEntry } from "../kernel";
import { Box } from "lucide-react";
import { label } from "../locale";
import { ModelEditor } from "../visual-assets/ModelEditor";
const entry: BundledPluginEntry = {
  id: "rh.model",
  create: (manifest) => ({
    manifest,
    views: [
      {
        id: "model",
        owner: manifest.id,
        label: label("方块模型", "Cuboid models"),
        icon: Box,
        tone: "blue",
        render: ModelEditor,
      },
    ],
  }),
};
export default entry;
