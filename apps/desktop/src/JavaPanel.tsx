import { t, formatNumber } from "./i18n";
import { PlusCircle } from "lucide-react";
import type { Api, JavaAddResult, JavaSelection, Settings } from "./types";
import { useJavaAction, useJavaCatalog } from "./javaManagement";
import { JavaDownloads } from "./JavaDownloads";

export function JavaPanel({
  settings,
  api,
  native,
  disabled,
  onSave,
  onRefresh,
  onNotify,
  downloadDisabled = disabled,
  onJavaTask = () => {},
  recommendation,
  onInstanceJava,
}: {
  settings: Settings;
  api: Api;
  native: boolean;
  disabled: boolean;
  onSave: (settings: Settings) => Promise<void>;
  onRefresh: () => Promise<void>;
  onNotify: (message: string) => void;
  downloadDisabled?: boolean;
  onJavaTask?: (id: string, reveal: boolean) => void;
  recommendation?: { name: string; major: number };
  onInstanceJava?: () => void;
}) {
  const { catalog, loading, error } = useJavaCatalog(api, settings);
  const action = useJavaAction({ api, settings, native, disabled, onNotify });
  const selected = settings.java || { mode: "auto" };
  const unavailable = [...catalog.unavailable];
  if (
    selected.mode === "manual" &&
    !catalog.runtimes.some((runtime) => runtime.path === selected.path) &&
    !unavailable.some((runtime) => runtime.path === selected.path)
  )
    unavailable.unshift({
      path: selected.path,
      error: loading ? t("java.checkingSelected") : t("java.selectedMissing"),
    });
  function select(java: JavaSelection) {
    void action.run(async () => {
      if (
        java.mode === selected.mode &&
        (java.mode === "auto" ||
          (selected.mode === "manual" && java.path === selected.path))
      )
        return;
      await onSave({ ...settings, java });
    });
  }
  return (
    <div className="ce-settings-panel">
      {recommendation && (
        <section className="ce-card">
          <p className="ce-java-status">
            {t("inspection.javaRecommendation", {
              name: recommendation.name,
              version: recommendation.major,
            })}
          </p>
          {onInstanceJava && (
            <button className="ce-button" onClick={onInstanceJava}>
              {t("inspection.instanceJava")}
            </button>
          )}
        </section>
      )}
      <div className="ce-card ce-java-add">
        <button
          disabled={action.disabled}
          onClick={() =>
            void action.run(async (isCurrent) => {
              const result = await api<JavaAddResult>("java_add", {
                revision: settings.revision,
              });
              if (!isCurrent()) return;
              if (result.status === "selected") await onRefresh();
              else if (result.status === "unavailable")
                onNotify(result.message || t("java.unavailableSelection"));
            })
          }
        >
          <PlusCircle size={20} />
          {t("ui.add")}
        </button>
      </div>
      <JavaDownloads
        key={`${settings.root_id || settings.root}:${recommendation?.major || 0}`}
        api={api}
        settings={settings}
        native={native}
        disabled={downloadDisabled}
        onNotify={onNotify}
        onTask={onJavaTask}
        recommendedMajor={recommendation?.major}
      />
      <section className="ce-card ce-java-list" aria-label={t("java.runtimes")}>
        <button
          className={`ce-java-auto ce-java-row ${selected.mode === "auto" ? "is-selected" : ""}`}
          aria-pressed={selected.mode === "auto"}
          disabled={action.disabled}
          onClick={() => select({ mode: "auto" })}
        >
          <strong>{t("java.auto")}</strong>
          <p>{t("java.autoHelp")}</p>
        </button>
        {catalog.runtimes.map((java) => (
          <button
            className={`ce-java-entry ce-java-row ${selected.mode === "manual" && selected.path === java.path ? "is-selected" : ""}`}
            key={java.path}
            aria-pressed={
              selected.mode === "manual" && selected.path === java.path
            }
            disabled={action.disabled}
            onClick={() => select({ mode: "manual", path: java.path })}
          >
            <div>JDK {formatNumber(java.major)}</div>
            <p>
              <span>{java.arch}</span> <span>{java.vendor}</span> {java.path}
            </p>
          </button>
        ))}
        {unavailable.map((java) => (
          <button
            className={`ce-java-entry ce-java-row ce-java-unavailable ${selected.mode === "manual" && selected.path === java.path ? "is-selected" : ""}`}
            key={java.path}
            aria-pressed={
              selected.mode === "manual" && selected.path === java.path
            }
            disabled
          >
            <div>{loading ? t("java.checking") : t("java.unavailable")}</div>
            <p>{java.path}</p>
            <p>{java.error}</p>
          </button>
        ))}
        {error && (
          <p className="ce-java-status" role="alert">
            {error}
          </p>
        )}
        {!error &&
          catalog.runtimes.length === 0 &&
          unavailable.length === 0 && (
            <p className="ce-java-status">
              {loading ? t("java.scanning") : t("java.none")}
            </p>
          )}
      </section>
    </div>
  );
}
