import type { BundledPluginEntry } from "../kernel";
import { FolderTree } from "lucide-react";
import { label } from "../locale";
import { Files } from "./files";
const entry: BundledPluginEntry = {
  id: "rh.files",
  create: (manifest) => ({
    manifest,
    views: [
      {
        id: "files",
        owner: manifest.id,
        label: label("文件图谱", "Project files"),
        icon: FolderTree,
        tone: "blue",
        render: Files,
      },
    ],
  }),
};
export default entry;
