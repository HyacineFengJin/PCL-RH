/** The same owner provides the homepage component and its native read service.
 * Discovery of its manifest does not import the component or start Code. */
import type { BundledPluginEntry } from "../kernel";
import { AnnouncementSurface } from "../../plugins/home/AnnouncementSurface";
export default {
  id: "launcher.home.announcements",
  create: (manifest, runtime) => ({
    manifest,
    launcherSurfaces: [
      {
        id: "launcher.home.announcements.feed",
        owner: manifest.id,
        slot: "home.announcements",
        render: AnnouncementSurface,
      },
    ],
    apply: async () => {
      await runtime.nativeServices?.activate(manifest.id);
    },
  }),
} satisfies BundledPluginEntry;
