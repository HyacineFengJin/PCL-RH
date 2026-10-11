import type { BundledPluginEntry } from "../kernel";
import { Network } from "lucide-react";
import { label } from "../locale";
import { Graph } from "./files";
const entry: BundledPluginEntry = {
  id: "rh.graph",
  create: (manifest) => ({
    manifest,
    views: [
      {
        id: "graph",
        owner: manifest.id,
        label: label("内容图谱", "Content graph"),
        icon: Network,
        render: Graph,
      },
    ],
  }),
};
export default entry;
