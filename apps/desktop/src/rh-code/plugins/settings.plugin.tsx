import type { BundledPluginEntry } from "../kernel";
import { preset } from "../layout";
import { label } from "../locale";
const entry: BundledPluginEntry = {
  id: "rh.settings",
  create: (manifest) => ({
    manifest,
    layouts: [
      ["conversation", label("创作", "Create")],
      ["parallel", label("并行对话", "Parallel conversations")],
      ["source", label("源码", "Source")],
    ].map(([name, text]) => ({
      id: `rh.settings.${name}`,
      owner: manifest.id,
      label: text,
      create: (project) =>
        preset(
          name,
          project.conversations.map((c) => c.id),
        ),
    })),
  }),
};
export default entry;
