import { Hammer } from "lucide-react";
import type { BundledPluginEntry } from "../kernel";
import { label } from "../locale";
import { BuildPanel } from "./BuildPanel";

const entry: BundledPluginEntry = {
  id: "rh.build",
  create: (manifest, runtime) => ({
    manifest,
    views: [
      {
        id: "build",
        owner: manifest.id,
        label: label("构建", "Build"),
        icon: Hammer,
        tone: "orange",
        render: BuildPanel,
      },
    ],
    commands: runtime.commands?.openBuild
      ? [
          {
            id: "build.open",
            owner: manifest.id,
            handler: runtime.commands.openBuild,
          },
        ]
      : [],
  }),
};
export default entry;
