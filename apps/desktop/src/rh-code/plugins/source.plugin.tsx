import type { BundledPluginEntry } from "../kernel";
import { FileCode2 } from "lucide-react";
import { label } from "../locale";
import { Source } from "./files";
const entry: BundledPluginEntry = {
  id: "rh.source",
  create: (manifest) => ({
    manifest,
    views: [
      {
        id: "source",
        owner: manifest.id,
        label: label("源码编辑", "Source editor"),
        icon: FileCode2,
        tone: "blue",
        render: Source,
      },
    ],
  }),
};
export default entry;
