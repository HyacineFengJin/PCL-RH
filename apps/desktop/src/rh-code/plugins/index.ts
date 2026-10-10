/** Only bundled, reviewed modules may register executable views. Community
 * manifests remain data and pass through the separate permission broker. */
import manifests from "../../../../../code/plugins.json";
import {
  MessageSquare,
  MessagesSquare,
  FolderTree,
  Network,
  FileCode2,
  Hammer,
  GitPullRequest,
  Terminal,
  ArrowRightLeft,
} from "lucide-react";
import type { Kernel, ProductPlugin } from "../kernel";
import type { Manifest, ViewContribution, ViewId } from "../types";
import { LegacyArchive } from "../LegacyArchive";
import { Chat, Conversations } from "./conversations";
import { Files, Graph, Source } from "./files";
import { Targets, Porting } from "./targets";
import { Changes, Output } from "./output";
import { label, viewNames } from "../locale";
const renderers = {
  legacy: LegacyArchive,
  chat: Chat,
  threads: Conversations,
  files: Files,
  graph: Graph,
  source: Source,
  targets: Targets,
  porting: Porting,
  changes: Changes,
  logs: Output,
};
const icons = {
  legacy: FolderTree,
  chat: MessageSquare,
  threads: MessagesSquare,
  files: FolderTree,
  graph: Network,
  source: FileCode2,
  targets: Hammer,
  porting: ArrowRightLeft,
  changes: GitPullRequest,
  logs: Terminal,
};
export function productPlugins(
  commands?: {
    run: (...args: any[]) => unknown;
    stop: (...args: any[]) => unknown;
  },
  owner?: () => Kernel,
): ProductPlugin[] {
  return (manifests as Manifest[]).map((manifest) => ({
    manifest,
    apply:
      manifest.id === "rh.agent" && commands && owner
        ? (ctx) => {
            owner().registerCommand(ctx, "agent.run", commands.run);
            owner().registerCommand(ctx, "agent.stop", commands.stop);
          }
        : undefined,
    views: manifest.views.map(
      (id: ViewId): ViewContribution => ({
        id,
        owner: manifest.id,
        label: label(...viewNames[id]),
        icon: icons[id],
        render: renderers[id],
      }),
    ),
  }));
}
