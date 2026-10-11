import type { BundledPluginEntry } from "../kernel";
import { Hammer } from "lucide-react";
import { label } from "../locale";
import { Targets } from "./targets";
const entry: BundledPluginEntry = {
  id: "rh.targets",
  create: (manifest) => ({
    manifest,
    views: [
      {
        id: "targets",
        owner: manifest.id,
        label: label("构建与测试", "Build and test"),
        icon: Hammer,
        render: Targets,
      },
    ],
  }),
};
export default entry;
