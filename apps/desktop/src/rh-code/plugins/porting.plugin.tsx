import type { BundledPluginEntry } from "../kernel";
import { ArrowRightLeft } from "lucide-react";
import { label } from "../locale";
import { Porting } from "./targets";
const entry: BundledPluginEntry = {
  id: "rh.porting",
  create: (manifest) => ({
    manifest,
    views: [
      {
        id: "porting",
        owner: manifest.id,
        label: label("版本迁移", "Version migration"),
        icon: ArrowRightLeft,
        tone: "orange",
        render: Porting,
      },
    ],
  }),
};
export default entry;
