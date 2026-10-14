import { useEffect, useRef, useState } from "react";
import { ChevronDown, Download, RefreshCw } from "lucide-react";
import { CeSelect } from "./CeSelect";
import { Collapse } from "./Collapse";
import { t } from "./i18n";
import { useJavaAction } from "./javaManagement";
import type { Api, Settings } from "./types";

export type JavaPackage = {
  component: string;
  sha1: string;
  version: string;
  major: number;
  platform: string;
  released: string;
};
const identity = (p: JavaPackage) => `${p.component}:${p.sha1}`;
function isCatalog(value: unknown): value is JavaPackage[] {
  return (
    Array.isArray(value) &&
    value.length <= 64 &&
    value.every(
      (p) =>
        p &&
        typeof p === "object" &&
        typeof p.component === "string" &&
        /^[a-z0-9-]{1,64}$/.test(p.component) &&
        typeof p.sha1 === "string" &&
        /^[a-f0-9]{40}$/.test(p.sha1) &&
        typeof p.version === "string" &&
        p.version.length > 0 &&
        p.version.length <= 128 &&
        typeof p.released === "string" &&
        p.released.length <= 128 &&
        ["linux", "linux-i386"].includes(p.platform) &&
        Number.isInteger(p.major) &&
        p.major > 0 &&
        p.major <= 999,
    )
  );
}

/** Catalog replies belong to one API/visible view. A submitted native job has
 * its own lifetime: even a late reply is tracked, but may not reopen this page. */
export function JavaDownloads({
  api,
  settings,
  native,
  disabled,
  onNotify,
  onTask,
  recommendedMajor,
}: {
  api: Api;
  settings: Settings;
  native: boolean;
  disabled: boolean;
  onNotify: (message: string) => void;
  onTask: (id: string, reveal: boolean) => void;
  recommendedMajor?: number;
}) {
  const [open, setOpen] = useState(!!recommendedMajor);
  const [refresh, setRefresh] = useState(0);
  const [choice, setChoice] = useState({ major: recommendedMajor, value: "" });
  const [catalog, setCatalog] = useState<{
    api: Api;
    packages: JavaPackage[];
    error: string;
    loading: boolean;
  } | null>(null);
  const current = useRef(api);
  current.current = api;
  const action = useJavaAction({
    api,
    settings,
    native,
    disabled,
    onNotify,
    context: String(recommendedMajor || 0),
  });
  useEffect(() => {
    if (recommendedMajor) setOpen(true);
  }, [recommendedMajor]);
  useEffect(() => {
    if (!open || !native) return;
    let live = true;
    setCatalog({ api, packages: [], error: "", loading: true });
    void api<JavaPackage[]>("java_download_catalog")
      .then((packages) => {
        if (!isCatalog(packages)) throw new Error(t("java.invalidData"));
        if (live && current.current === api)
          setCatalog({ api, packages, loading: false, error: "" });
      })
      .catch((error) => {
        if (live && current.current === api)
          setCatalog({
            api,
            packages: [],
            loading: false,
            error: String(error),
          });
      });
    return () => {
      live = false;
    };
  }, [api, native, open, refresh]);
  const result = catalog?.api === api ? catalog : null;
  const packages = result?.packages || [];
  // A diagnostic recommendation must not quietly choose a different Java
  // major when the official catalog has no matching runtime.
  const selected =
    (choice.major === recommendedMajor &&
      packages.find((p) => identity(p) === choice.value)) ||
    (recommendedMajor
      ? packages.find((p) => p.major === recommendedMajor)
      : packages[0]);
  return (
    <section className="ce-card ce-java-download">
      <button
        className="ce-java-download-heading"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
      >
        <Download size={20} />
        <span>{t("java.download")}</span>
        <ChevronDown size={17} className={open ? "is-open" : ""} />
      </button>
      <Collapse open={open}>
        <div className="ce-java-download-body">
          <p className="ce-java-status">{t("java.downloadHelp")}</p>
          {result?.error && (
            <p className="ce-java-status auth-error" role="alert">
              {result.error}
            </p>
          )}
          <label className="ce-settings-row">
            <span>{t("java.version")}</span>
            <CeSelect
              className="ce-field"
              value={selected ? identity(selected) : ""}
              disabled={
                !packages.length || !!result?.loading || action.disabled
              }
              onChange={(e) =>
                setChoice({ major: recommendedMajor, value: e.target.value })
              }
            >
              {!selected && packages.length > 0 && (
                <option value="" disabled>
                  {t("java.chooseVersion")}
                </option>
              )}
              {packages.length ? (
                packages.map((p) => (
                  <option key={identity(p)} value={identity(p)}>
                    Java {p.major} · {p.version}
                  </option>
                ))
              ) : (
                <option value="">
                  {result?.loading || !result
                    ? t("java.loadingOfficial")
                    : t("java.noneOfficial")}
                </option>
              )}
            </CeSelect>
          </label>
          <div className="ce-actions">
            <button
              className="ce-button"
              disabled={!selected || action.disabled || !!result?.loading}
              onClick={() =>
                void action.run(async (isCurrent) => {
                  if (!selected) return;
                  const submitted = await api<{ id: string }>(
                    "java_download_start",
                    {
                      component: selected.component,
                      sha1: selected.sha1,
                      revision: settings.revision,
                    },
                  );
                  if (
                    !submitted ||
                    typeof submitted.id !== "string" ||
                    !submitted.id ||
                    submitted.id.length > 256
                  )
                    throw new Error(t("java.invalidData"));
                  onTask(submitted.id, isCurrent());
                })
              }
            >
              <Download size={17} />
              {t("java.startDownload")}
            </button>
            <button
              className="ce-button"
              disabled={!!result?.loading || !native}
              onClick={() => setRefresh((n) => n + 1)}
            >
              <RefreshCw size={17} />
              {t("java.refreshOfficial")}
            </button>
          </div>
        </div>
      </Collapse>
    </section>
  );
}
