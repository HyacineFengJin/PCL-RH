/** External packages contribute data to this trusted view. They never receive
 * the controller, project files, native IPC or an executable render callback. */
import { ListChecks, Puzzle } from "lucide-react";
import { useEffect, useState } from "react";
import { ExperimentalCards } from "../../ExperimentalExtensions";
import { ExtensionActions } from "../../ExtensionActions";
import { serviceError } from "../../i18n";
import type {
  ExperimentalNavigate,
  ExtensionCardFilter,
} from "../../experimentalTypes";
import { useCode } from "../controller";
import type { BundledPluginEntry, PluginRuntime } from "../kernel";
import { label } from "../locale";
import { useCodeSurfaceVisible } from "../surface";
type CatalogFeedback = { error: string; listeners: Set<() => void> };
type PanelRender = {
  filter: ExtensionCardFilter;
  render: () => React.JSX.Element;
};
/** The controller's Api wrapper is stable across launcher Api replacements.
 * The host port closes availability during render, then notifies in its effect;
 * the child card/action scope sees false -> true even for the same wrapper. */
function useCommunityAvailable(community: PluginRuntime["community"]) {
  const [, redraw] = useState(0);
  useEffect(
    () => community?.subscribe(() => redraw((value) => value + 1)),
    [community],
  );
  return !community || community.isAvailable();
}

export function CommunityCards({
  onNavigate,
  filter,
  metadata,
  community,
}: {
  onNavigate: ExperimentalNavigate;
  filter?: ExtensionCardFilter;
  metadata?: CatalogFeedback;
  community?: PluginRuntime["community"];
}) {
  const c = useCode(),
    visible = useCodeSurfaceVisible();
  const available = useCommunityAvailable(community);
  const [, redraw] = useState(0);
  useEffect(() => {
    const refresh = () => redraw((value) => value + 1);
    metadata?.listeners.add(refresh);
    return () => {
      metadata?.listeners.delete(refresh);
    };
  }, [metadata]);
  return (
    <div className="rc-panel-pad rc-community-cards">
      {metadata?.error && (
        <p className="experimental-error" role="alert">
          {metadata.error}
        </p>
      )}
      <ExperimentalCards
        api={c.api}
        native={c.native && visible && available}
        slot="rh-code.cards"
        filter={filter}
        refreshable={!filter}
        onNavigate={onNavigate}
        emptyText={label(
          "暂无启用的工作区卡片，可通过顶部橙色入口导入插件。",
          "No workspace cards are enabled. Import a plugin from the orange entry above.",
        )}
      />
    </div>
  );
}

function CommunityActions({
  onNavigate,
  community,
}: {
  onNavigate: ExperimentalNavigate;
  community?: PluginRuntime["community"];
}) {
  const c = useCode(),
    visible = useCodeSurfaceVisible();
  const available = useCommunityAvailable(community);
  return (
    <div className="rc-panel-pad rc-community-cards">
      <ExtensionActions
        api={c.api}
        native={c.native && visible && available}
        onNavigate={onNavigate}
      />
    </div>
  );
}

const entry: BundledPluginEntry = {
  id: "rh.extensions",
  create: (manifest, runtime) => {
    const metadata: CatalogFeedback = { error: "", listeners: new Set() };
    const metadataError = (error: string) => {
      if (metadata.error === error) return;
      metadata.error = error;
      for (const listener of [...metadata.listeners]) listener();
    };
    return {
      manifest,
      views: [
        {
          id: "rh.extensions.cards",
          owner: manifest.id,
          label: label("插件卡片", "Plugin cards"),
          icon: Puzzle,
          tone: "orange",
          render: () => (
            <CommunityCards
              metadata={metadata}
              community={runtime.community}
              onNavigate={runtime.navigate ?? (() => {})}
            />
          ),
        },
        {
          id: "rh.extensions.actions",
          owner: manifest.id,
          label: label("插件操作", "Plugin actions"),
          icon: ListChecks,
          tone: "orange",
          render: () => (
            <CommunityActions
              community={runtime.community}
              onNavigate={runtime.navigate ?? (() => {})}
            />
          ),
        },
      ],
      apply(ctx, { replaceViews }) {
        const community = runtime.community;
        if (!community) return;
        let renders = new Map<string, PanelRender>();
        ctx.effect(() => {
          let alive = true,
            epoch = 0;
          const safePublish = (views: Parameters<typeof replaceViews>[0]) => {
            try {
              replaceViews(views);
              return true;
            } catch (error) {
              // configure() closes admission before queued Cordis cleanup runs.
              // A surface event can still reach that old subscription in between.
              if (
                error instanceof Error &&
                error.message === `Plugin views unavailable: ${manifest.id}`
              ) {
                // A newer configure() may cancel that queued retirement and
                // keep this fiber alive. Only Cordis cleanup ends its lifetime.
                return false;
              }
              throw error;
            }
          };
          const refresh = () => {
            if (!alive) return;
            const owner = ++epoch;
            if (!community.isAvailable()) {
              renders.clear();
              safePublish([]);
              return;
            }
            void community
              .readCatalog()
              .then((catalog) => {
                if (!alive || owner !== epoch || !community.isAvailable())
                  return;
                const nextRenders = new Map<string, PanelRender>();
                if (
                  safePublish(
                    catalog.panels.map((panel) => {
                      let cached = renders.get(panel.viewId);
                      if (
                        !cached ||
                        cached.filter.extensionId !== panel.extensionId ||
                        cached.filter.cardId !== panel.cardId
                      ) {
                        const filter = {
                          extensionId: panel.extensionId,
                          cardId: panel.cardId,
                        };
                        cached = {
                          filter,
                          render: () => (
                            <CommunityCards
                              filter={filter}
                              community={runtime.community}
                              onNavigate={runtime.navigate ?? (() => {})}
                            />
                          ),
                        };
                      }
                      nextRenders.set(panel.viewId, cached);
                      return {
                        id: panel.viewId,
                        owner: manifest.id,
                        label:
                          panel.title === panel.extensionName
                            ? panel.title
                            : `${panel.title} · ${panel.extensionName}`,
                        icon: Puzzle,
                        tone: "orange" as const,
                        render: cached.render,
                      };
                    }),
                  )
                ) {
                  renders = nextRenders;
                  metadataError("");
                }
              })
              .catch((error) => {
                if (alive && owner === epoch && community.isAvailable()) {
                  // A failed metadata read is not a removal. Keep the last
                  // render identities and their dirty drafts until a successful
                  // projection proves removal, or the host retires the surface.
                  metadataError(serviceError(error));
                }
              });
          };
          const unsubscribe = community.subscribe(refresh);
          refresh();
          return () => {
            alive = false;
            epoch++;
            unsubscribe();
            renders.clear();
          };
        });
      },
    };
  },
};
export default entry;
