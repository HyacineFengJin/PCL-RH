/** Community browsing keeps provider IDs in their own namespace. Aliases come
 * from the backend's verified project links; a URL-only alias opens its page
 * and must never be promoted into an installable provider ID by the UI. */
export type CommunitySource = "Modrinth" | "CurseForge" | "MC百科";
export type CommunitySourceAlias = {
  source: CommunitySource;
  project_id: string | null;
  url: string;
};
export type CommunityIdentity = {
  source?: string;
  project_id: string;
  project_type?: string;
  url?: string;
  identity_key?: string;
  sources?: CommunitySourceAlias[];
};

export function communitySource(value?: string): CommunitySource | null {
  switch (value?.toLowerCase()) {
    case "modrinth":
      return "Modrinth";
    case "curseforge":
      return "CurseForge";
    case "mc百科":
    case "mcmod":
      return "MC百科";
    default:
      return null;
  }
}

function aliasUrlKey(alias: CommunitySourceAlias): string | null {
  try {
    const url = new URL(alias.url);
    const hosts = {
      Modrinth: ["modrinth.com", "www.modrinth.com"],
      CurseForge: ["www.curseforge.com", "curseforge.com"],
      MC百科: ["www.mcmod.cn", "mcmod.cn", "m.mcmod.cn"],
    };
    if (
      !["http:", "https:"].includes(url.protocol) ||
      !hosts[alias.source].includes(url.hostname) ||
      url.username ||
      url.password ||
      url.port
    )
      return null;
    return `${alias.source}:${url.pathname.replace(/\/+$/, "")}`;
  } catch {
    return null;
  }
}

export function communityAliases(
  resource: CommunityIdentity,
  ...additional: (CommunitySourceAlias[] | undefined)[]
): CommunitySourceAlias[] {
  const source = communitySource(resource.source ?? "Modrinth");
  const candidates = [
    ...(source && resource.project_id
      ? [{ source, project_id: resource.project_id, url: resource.url ?? "" }]
      : []),
    ...(resource.sources ?? []),
    ...additional.flatMap((aliases) => aliases ?? []),
  ];
  const identified = new Map<string, CommunitySourceAlias>();
  const urlIdentities = new Map<string, Set<string>>();
  // Details can resolve a search link's slug into a real provider ID. Index
  // every observed URL before rendering so either arrival order upgrades the
  // URL-only entry. A shared URL never merges two distinct real provider IDs.
  for (const alias of candidates) {
    if (!alias.project_id) continue;
    const key = `${alias.source}:${alias.project_id}`;
    const previous = identified.get(key);
    if (!previous || (!previous.url && alias.url)) identified.set(key, alias);
    const urlKey = aliasUrlKey(alias);
    if (urlKey) {
      const identities = urlIdentities.get(urlKey) ?? new Set<string>();
      identities.add(key);
      urlIdentities.set(urlKey, identities);
    }
  }
  const rendered = new Set<string>();
  const aliases: CommunitySourceAlias[] = [];
  for (const candidate of candidates) {
    const urlKey = aliasUrlKey(candidate);
    const resolved = urlKey ? urlIdentities.get(urlKey) : undefined;
    // Ambiguous URL-only actions are redundant; keep each real ID separately
    // without assigning the search link to either project's identity.
    if (!candidate.project_id && resolved && resolved.size > 1) continue;
    const key = candidate.project_id
      ? `${candidate.source}:${candidate.project_id}`
      : (resolved?.values().next().value ??
        `${candidate.source}:url:${urlKey ?? candidate.url}`);
    if (rendered.has(key)) continue;
    rendered.add(key);
    aliases.push(identified.get(key) ?? candidate);
  }
  return aliases;
}

/** Provider icon hosts are independent from project/action URLs. Never accept
 * arbitrary remote images, data URLs or an encyclopedia outgoing redirect. */
export function communityIcon(value?: string | null): string | undefined {
  try {
    const url = new URL(value ?? "");
    return url.protocol === "https:" &&
      [
        "cdn.modrinth.com",
        "media.forgecdn.net",
        "mediafilez.forgecdn.net",
        "i.mcmod.cn",
        "www.mcmod.cn",
      ].includes(url.hostname) &&
      !url.username &&
      !url.password &&
      !url.port
      ? url.href
      : undefined;
  } catch {
    return undefined;
  }
}

export function mcmodResourceUrl(
  title: string,
  aliases: CommunitySourceAlias[],
): string {
  const encyclopedia = aliases.find(
    (alias) => alias.source === "MC百科" && alias.url,
  );
  return (
    encyclopedia?.url ??
    `https://search.mcmod.cn/s?key=${encodeURIComponent(title)}`
  );
}
