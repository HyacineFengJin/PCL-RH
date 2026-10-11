import type { BundledPluginEntry } from "../kernel";
const entry: BundledPluginEntry = {
  id: "rh.agent",
  create: (manifest, runtime) => ({
    manifest,
    commands: runtime.commands
      ? [
          {
            id: "agent.run",
            owner: manifest.id,
            handler: runtime.commands.run,
          },
          {
            id: "agent.stop",
            owner: manifest.id,
            handler: runtime.commands.stop,
          },
        ]
      : [],
  }),
};
export default entry;
