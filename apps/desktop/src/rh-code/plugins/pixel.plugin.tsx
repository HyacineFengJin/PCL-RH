import type { BundledPluginEntry } from "../kernel";
import { Paintbrush } from "lucide-react";
import { label } from "../locale";
import { PixelEditor } from "../visual-assets/PixelEditor";
const entry: BundledPluginEntry = {
  id: "rh.pixel",
  create: (manifest) => ({
    manifest,
    views: [
      {
        id: "pixel",
        owner: manifest.id,
        label: label("像素材质", "Pixel textures"),
        icon: Paintbrush,
        tone: "purple",
        render: PixelEditor,
      },
    ],
  }),
};
export default entry;
