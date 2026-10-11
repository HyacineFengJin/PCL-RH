import type { BundledPluginEntry } from "../kernel";
import { GitPullRequest } from "lucide-react";
import { label } from "../locale";
import { Changes } from "./output";
const entry: BundledPluginEntry = {
  id: "rh.changes",
  create: (manifest) => ({
    manifest,
    views: [
      {
        id: "changes",
        owner: manifest.id,
        label: label("改动审阅", "Changes and review"),
        icon: GitPullRequest,
        tone: "orange",
        render: Changes,
      },
    ],
  }),
};
export default entry;
