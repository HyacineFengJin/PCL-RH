import type { BundledPluginEntry } from "../kernel";
import { Terminal } from "lucide-react";
import { label } from "../locale";
import { Output } from "./output";
const entry: BundledPluginEntry = {
  id: "rh.tasks",
  create: (manifest) => ({
    manifest,
    views: [
      {
        id: "logs",
        owner: manifest.id,
        label: label("任务与输出", "Tasks and output"),
        icon: Terminal,
        render: Output,
      },
    ],
  }),
};
export default entry;
