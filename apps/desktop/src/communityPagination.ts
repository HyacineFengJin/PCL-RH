/** Bounded, query-owned loaded pages. Native provider metadata controls Next;
 * visible-card counts may shrink after verified cross-source deduplication.
 * Keeping raw pages makes revisiting a page independent of a global seen set.
 */
import type { ResourceSummary } from "./ResourceDetails";
import { communityAliases, communityAliasUrlKey } from "./communityResources";

export type CommunityPage = {
  hits: ResourceSummary[];
  warnings: string[];
  pagination?: { page: number; has_next: boolean };
};

type Identity = {
  ids: Map<string, Set<string>>;
  keys: Set<string>;
  ambiguous: boolean;
};
type Loaded = { result: CommunityPage; identities: Identity[] };
const MAX_LOADED_PAGES = 8;

function identity(hit: ResourceSummary): Identity {
  const ids = new Map<string, Set<string>>(),
    keys = new Set<string>(),
    urls = new Map<string, Set<string>>();
  for (const alias of communityAliases(hit)) {
    if (alias.project_id) {
      const sourceIds = ids.get(alias.source) ?? new Set<string>();
      sourceIds.add(alias.project_id);
      ids.set(alias.source, sourceIds);
      keys.add(`${alias.source}:id:${alias.project_id}`);
    }
    const url = communityAliasUrlKey(alias);
    if (url) {
      keys.add(`url:${url}`);
      const sourceUrls = urls.get(alias.source) ?? new Set<string>();
      sourceUrls.add(url);
      urls.set(alias.source, sourceUrls);
    }
  }
  return {
    ids,
    keys,
    ambiguous: [...urls.values()].some((urls) => urls.size > 1),
  };
}

function sameProject(left: Identity, right: Identity): boolean {
  // A broad encyclopedia entry may link multiple forks from one provider.
  // Across independently loaded pages, retaining those cards is safer than
  // hiding a newly encountered fork using only one side of that evidence.
  if (left.ambiguous || right.ambiguous) return false;
  // A shared URL must never hide two distinct real IDs from the same provider.
  for (const source of new Set([...left.ids.keys(), ...right.ids.keys()])) {
    if (
      new Set([
        ...(left.ids.get(source) ?? []),
        ...(right.ids.get(source) ?? []),
      ]).size > 1
    )
      return false;
  }
  return [...left.keys].some((key) => right.keys.has(key));
}

export class CommunityPages {
  private pages = new Map<number, Loaded>();

  get(page: number): CommunityPage | undefined {
    return this.pages.get(page)?.result;
  }

  remember(page: number, result: CommunityPage) {
    this.pages.delete(page);
    this.pages.set(page, { result, identities: result.hits.map(identity) });
    while (this.pages.size > MAX_LOADED_PAGES)
      this.pages.delete(this.pages.keys().next().value!);
  }

  visibleHits(page: number): ResourceSummary[] {
    const loaded = this.pages.get(page);
    if (!loaded) return [];
    const earlier = [...this.pages.entries()]
      .filter(([index]) => index < page)
      .flatMap(([, entry]) => entry.identities);
    return loaded.result.hits.filter(
      (_, index) =>
        !earlier.some((previous) =>
          sameProject(previous, loaded.identities[index]),
        ),
    );
  }
}
