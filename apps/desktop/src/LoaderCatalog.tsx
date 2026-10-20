import { t, formatNumber, type MessageKey } from "./i18n";
import { useEffect, useState } from "react";
import {
  Box,
  ChevronDown,
  FlaskConical,
  Gauge,
  ScrollText,
} from "lucide-react";
import type { Api } from "./types";
import { Collapse } from "./Collapse";
import forge from "./assets/game-icons/forge.svg";
import grass from "./assets/game-icons/grass.svg";
import neoForge from "./assets/game-icons/neoforge.svg";
export { Favorites } from "./LauncherFavorites";
type Group = { minecraft: string; versions: string[]; labelKey?: MessageKey };
type VersionEntry = { id: string; kind: string; release_time: string };
export const installerPages = [
  "installer-minecraft",
  "OptiFine",
  "Forge",
  "NeoForge",
  "Cleanroom",
  "Fabric",
  "Legacy Fabric",
  "LabyMod",
  "LiteLoader",
];
const providers: Record<
  string,
  { name: string; introKey: MessageKey; website: string }
> = {
  "installer-minecraft": {
    name: "Minecraft",
    introKey: "loader.minecraftIntro",
    website: "https://www.minecraft.net/",
  },
  NeoForge: {
    name: "NeoForge",
    introKey: "loader.neoforgeIntro",
    website: "https://neoforged.net/",
  },
  Forge: {
    name: "Forge",
    introKey: "loader.forgeIntro",
    website: "https://files.minecraftforge.net/",
  },
  OptiFine: {
    name: "OptiFine",
    introKey: "loader.optifineIntro",
    website: "https://optifine.net/downloads",
  },
  Cleanroom: {
    name: "Cleanroom",
    introKey: "loader.cleanroomIntro",
    website: "https://cleanroommc.com/",
  },
  Fabric: {
    name: "Fabric",
    introKey: "loader.fabricIntro",
    website: "https://fabricmc.net/",
  },
  "Legacy Fabric": {
    name: "Legacy Fabric",
    introKey: "loader.legacyFabricIntro",
    website: "https://legacyfabric.net/",
  },
  LabyMod: {
    name: "LabyMod",
    introKey: "loader.labyIntro",
    website: "https://laby.net/client",
  },
  LiteLoader: {
    name: "LiteLoader",
    introKey: "loader.liteloaderIntro",
    website: "https://www.liteloader.com/",
  },
};
const cache = new Map<string, { groups: Group[]; time: number }>();
function LoaderIcon({ loader }: { loader: string }) {
  const image =
    loader === "NeoForge"
      ? neoForge
      : loader === "Forge"
        ? forge
        : loader === "installer-minecraft"
          ? grass
          : undefined;
  const Icon =
    loader === "OptiFine"
      ? Gauge
      : loader.includes("Fabric")
        ? ScrollText
        : loader === "Cleanroom"
          ? FlaskConical
          : Box;
  return image ? (
    <img className="instance-icon" src={image} alt="" />
  ) : (
    <span className="instance-icon ce-loader-icon">
      <Icon size={28} strokeWidth={1.5} />
    </span>
  );
}
export function LoaderCatalog({
  api,
  loader,
  catalog,
  catalogLoading,
}: {
  api: Api;
  loader: string;
  catalog: VersionEntry[];
  catalogLoading: boolean;
}) {
  const [groups, setGroups] = useState<Group[]>([]),
    [expanded, setExpanded] = useState<string[]>([]),
    [error, setError] = useState(""),
    [loading, setLoading] = useState(true),
    [retry, setRetry] = useState(0);
  const provider = providers[loader];
  const minecraft = loader === "installer-minecraft";
  const websiteOnly = loader === "LabyMod";
  useEffect(() => {
    if (minecraft || websiteOnly) {
      setLoading(false);
      return;
    }
    let live = true;
    const cached = cache.get(loader);
    if (!retry && cached && Date.now() - cached.time < 300_000) {
      setGroups(cached.groups);
      setLoading(false);
      return;
    }
    setLoading(true);
    setError("");
    api<Group[]>("loader_catalog", { loader })
      .then((groups) => {
        cache.set(loader, { groups, time: Date.now() });
        if (live) setGroups(groups);
      })
      .catch((e) => {
        if (live) setError(String(e));
      })
      .finally(() => {
        if (live) setLoading(false);
      });
    return () => {
      live = false;
    };
  }, [api, loader, minecraft, websiteOnly, retry]);
  const shownGroups: Group[] = minecraft
    ? [
        {
          minecraft: "vanilla-release",
          labelKey: "download.release" as const,
          versions: catalog
            .filter((v) => v.kind === "release")
            .map((v) => v.id),
        },
        {
          minecraft: "vanilla-preview",
          labelKey: "download.preview" as const,
          versions: catalog
            .filter((v) => v.kind === "snapshot")
            .map((v) => v.id),
        },
        {
          minecraft: "vanilla-ancient",
          labelKey: "download.ancient" as const,
          versions: catalog
            .filter((v) => !["release", "snapshot"].includes(v.kind))
            .map((v) => v.id),
        },
      ].filter((g) => g.versions.length)
    : groups;
  return (
    <>
      <section className="ce-card ce-loader-intro">
        <h2 className="ce-card-title">
          {provider.name} {t("ui.introduction")}
        </h2>
        <p>
          {t(provider.introKey)}
          <br />
          {minecraft
            ? t("loader.minecraftFiles")
            : websiteOnly
              ? t("loader.linuxInstaller")
              : t("loader.manualHelp", { name: provider.name })}
        </p>
        <button
          className="ce-button primary"
          onClick={() => {
            setError("");
            api("ui_open_link", { url: provider.website }).catch((e) =>
              setError(String(e)),
            );
          }}
        >
          {t("ui.website")}
        </button>
      </section>
      {(minecraft ? catalogLoading : loading) ? (
        <section className="ce-card">
          <p className="ce-empty" role="status">
            {t("loader.loading")}
          </p>
        </section>
      ) : error ? (
        <section className="ce-card ce-loader-error">
          <p className="ce-empty" role="alert">
            {error}
          </p>
          <button className="ce-button" onClick={() => setRetry((v) => v + 1)}>
            {t("ui.reload")}
          </button>
        </section>
      ) : websiteOnly ? (
        <section className="ce-card">
          <h2 className="ce-card-title">{t("loader.installer")}</h2>
          <p className="ce-empty">{t("loader.labyInstaller")}</p>
          <button
            className="ce-button primary"
            onClick={() =>
              api("ui_open_link", { url: provider.website }).catch((e) =>
                setError(String(e)),
              )
            }
          >
            {t("loader.downloadWebsite")}
          </button>
        </section>
      ) : !shownGroups.length ? (
        <section className="ce-card">
          <p className="ce-empty">{t("loader.empty")}</p>
        </section>
      ) : (
        shownGroups.map((group) => (
          <section className="ce-card ce-loader-group" key={group.minecraft}>
            <button
              className="ce-version-group-toggle"
              aria-expanded={expanded.includes(group.minecraft)}
              onClick={() =>
                setExpanded((v) =>
                  v.includes(group.minecraft)
                    ? v.filter((x) => x !== group.minecraft)
                    : [...v, group.minecraft],
                )
              }
            >
              {group.labelKey
                ? t(group.labelKey)
                : group.minecraft === "安装器"
                  ? t("loader.installer")
                  : group.minecraft}{" "}
              ({formatNumber(group.versions.length)})
              <ChevronDown
                size={17}
                className={`ce-disclosure-arrow ${expanded.includes(group.minecraft) ? "is-open" : ""}`}
              />
            </button>
            <Collapse open={expanded.includes(group.minecraft)}>
              {group.versions.map((v) => (
                <button
                  className="ce-loader-entry"
                  disabled
                  title={
                    minecraft
                      ? t("loader.singleFileUnavailable")
                      : t("loader.downloadUnavailable")
                  }
                  key={v}
                >
                  <LoaderIcon loader={loader} />
                  <div>
                    {provider.name}{" "}
                    {loader === "OptiFine" ? v.replaceAll("_", " ") : v}
                    <small>
                      {minecraft
                        ? group.labelKey
                          ? t(group.labelKey)
                          : group.minecraft
                        : group.minecraft === "安装器"
                          ? t("loader.installer")
                          : `Minecraft ${group.minecraft}`}
                    </small>
                  </div>
                </button>
              ))}
            </Collapse>
          </section>
        ))
      )}
    </>
  );
}
