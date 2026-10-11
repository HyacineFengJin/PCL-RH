import type { BundledPluginEntry } from "../kernel";
const entry: BundledPluginEntry = {
  id: "rh.project",
  create: (manifest) => ({
    manifest,
  }),
};
export default entry;
