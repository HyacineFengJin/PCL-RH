import { CeSelect } from "./CeSelect";
import { createContext, useContext, useEffect, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";
import type { LauncherPreferencePanelProps } from "./launcherTypes";
import { defaultLauncherPreferenceView } from "./launcherTypes";
import { LauncherPersonalize, LauncherLanguage } from "./LauncherPersonalize";
import { LauncherMisc } from "./LauncherMisc";
import { LauncherLogs, type LauncherLogInitialSelection } from "./LauncherLogs";
import { LauncherUpdates } from "./LauncherUpdates";
import {
  LauncherCard as Card,
  LauncherCheck,
  LauncherSelect,
  LauncherRange,
  useLauncherPreferenceEditor,
} from "./LauncherSettingsControls";
import { createTranslator } from "./i18n";
import { ArrowUp } from "lucide-react";
import type { Api, Settings } from "./types";
import commandIcon from "./assets/game-icons/command.png";
import lampTexture from "./assets/game-icons/redstone-lamp.png";
import launcherIcon from "../../../assets/pcl-rh.png";
import { LegalDocuments } from "./LegalDocuments";

const TranslationContext = createContext(createTranslator());
function Field({
  label,
  value,
  children,
  plain = false,
  reason,
}: {
  label: string;
  value?: string;
  children?: React.ReactNode;
  plain?: boolean;
  reason?: string;
}) {
  const tr = useContext(TranslationContext);
  return (
    <label className="extra-row">
      <span>{label}</span>
      {children ||
        (plain ? (
          <span className="extra-muted">{value}</span>
        ) : (
          <CeSelect
            className="ce-field"
            disabled
            title={reason || tr.t("common.unavailable")}
          >
            <option>{value}</option>
          </CeSelect>
        ))}
    </label>
  );
}
function Check({
  children,
  checked = false,
  reason,
}: {
  children: React.ReactNode;
  checked?: boolean;
  reason?: string;
}) {
  const tr = useContext(TranslationContext);
  return (
    <label className="ce-check extra-check">
      <input
        type="checkbox"
        checked={checked}
        disabled
        readOnly
        title={reason || tr.t("common.unavailable")}
      />
      {children}
    </label>
  );
}
const project = "https://github.com/HyacineFengJin/PCL-RH";
const NotifyContext = createContext<(message: string) => void>(() => {});
function LinkButton({
  children,
  url,
  api,
  primary = false,
}: {
  children: React.ReactNode;
  url: string;
  api: Api;
  primary?: boolean;
}) {
  const notify = useContext(NotifyContext);
  const tr = useContext(TranslationContext);
  return (
    <button
      className={"ce-button " + (primary ? "primary" : "")}
      onClick={() =>
        api("ui_open_link", { url }).catch((e) => notify(tr.serviceError(e)))
      }
    >
      {children}
    </button>
  );
}
export type ExtraSettingsProps = LauncherPreferencePanelProps & {
  section: string;
  settings: Settings;
  api: Api;
  rootApi?: Api;
  initialLog?: LauncherLogInitialSelection;
  native?: boolean;
  revealHidden?: boolean;
  onOpen: (kind: string) => void;
};
export function ExtraSettings(props: ExtraSettingsProps) {
  const tr = createTranslator(
    (props.preferences || defaultLauncherPreferenceView()).preferences
      .localization,
  );
  return (
    <TranslationContext.Provider value={tr}>
      <NotifyContext.Provider value={props.onNotify}>
        <ExtraSettingsContent {...props} />
      </NotifyContext.Provider>
    </TranslationContext.Provider>
  );
}
function ExtraSettingsContent({
  section,
  settings,
  api,
  rootApi,
  initialLog,
  native = isTauri(),
  onOpen,
  onNotify,
  preferences,
  onPatch,
  preferenceBusy,
  supportedEffects,
  onLauncherAction,
  fontFamilies,
  revealHidden,
}: ExtraSettingsProps) {
  const preferenceProps = {
    preferences,
    onPatch,
    preferenceBusy,
    supportedEffects,
    onLauncherAction,
    fontFamilies,
    onNotify,
  };
  const preferenceEditor = useLauncherPreferenceEditor(preferenceProps);
  const tr = useContext(TranslationContext);
  const [feedback, setFeedback] = useState<Issue[]>([]),
    [feedbackError, setFeedbackError] = useState(""),
    [loading, setLoading] = useState(false);
  const [contributors, setContributors] = useState<Contributor[]>([]);
  const [contributorError, setContributorError] = useState("");
  useEffect(() => {
    let live = true;
    if (section === "feedback") {
      setLoading(true);
      api<Issue[]>("project_feedback")
        .then((v) => {
          if (live) {
            setFeedback(v);
            setFeedbackError("");
          }
        })
        .catch((e) => {
          if (live) setFeedbackError(String(e));
        })
        .finally(() => {
          if (live) setLoading(false);
        });
    }
    if (section === "about") {
      api<Contributor[]>("upstream_contributors")
        .then((v) => {
          if (live) {
            setContributors(v);
            setContributorError("");
          }
        })
        .catch((e) => {
          if (live) setContributorError(String(e));
        });
    }
    return () => {
      live = false;
    };
  }, [section, api, settings.root]);
  if (section === "manage")
    return (
      <div className="extra-settings">
        <Card title={tr.t("manage.gameResources")}>
          <div className="extra-fields">
            <Field
              label={tr.t("manage.fileSource")}
              value={tr.t("manage.official")}
              reason={tr.t("manage.sourceUnavailable")}
            />
            <Field
              label={tr.t("manage.versionSource")}
              value={tr.t("manage.official")}
              reason={tr.t("manage.sourceUnavailable")}
            />
            <Field label={tr.t("manage.threads")}>
              <LauncherRange
                label={tr.t("manage.threads")}
                value={
                  preferenceEditor.prefs.management.max_concurrent_transfers
                }
                min={1}
                max={64}
                step={1}
                formatNumber={tr.formatNumber}
                disabled={preferenceEditor.disabled("download_policy")}
                reason={preferenceEditor.reason(
                  "download_policy",
                  tr.t("manage.threadsUnavailable"),
                )}
                onCommit={(max_concurrent_transfers) =>
                  preferenceEditor.patch("download_policy", {
                    management: { max_concurrent_transfers },
                  })
                }
              />
            </Field>
            <Field label={tr.t("manage.speed")}>
              <LauncherRange
                label={tr.t("manage.speed")}
                value={
                  preferenceEditor.prefs.management
                    .total_rate_limit_mib_per_second
                }
                min={0}
                max={1024}
                step={1}
                unit=" MiB/s"
                formatNumber={tr.formatNumber}
                disabled={preferenceEditor.disabled("download_policy")}
                reason={preferenceEditor.reason(
                  "download_policy",
                  tr.t("manage.speedUnavailable"),
                )}
                onCommit={(total_rate_limit_mib_per_second) =>
                  preferenceEditor.patch("download_policy", {
                    management: { total_rate_limit_mib_per_second },
                  })
                }
              />
            </Field>
            <Field
              label={tr.t("manage.target")}
              plain
              value={tr.t("manage.targetHelp")}
            />
            <Field label={tr.t("manage.install")}>
              <div className="extra-two">
                <LauncherCheck
                  value={preferenceEditor.prefs.auto_select_installed}
                  disabled={preferenceEditor.disabled("auto_select_installed")}
                  reason={preferenceEditor.reason(
                    "auto_select_installed",
                    tr.t("manage.autoSelectUnavailable"),
                  )}
                  onChange={(auto_select_installed) =>
                    preferenceEditor.patch("auto_select_installed", {
                      auto_select_installed,
                    })
                  }
                >
                  {tr.t("manage.autoSelect")}
                </LauncherCheck>
                <Check reason={tr.t("manage.authlibUnavailable")}>
                  {tr.t("manage.authlib")}
                </Check>
              </div>
            </Field>
          </div>
          <p className="launcher-setting-summary">
            {tr.t("manage.transferPolicyHelp")}
          </p>
        </Card>
        <Card title={tr.t("manage.communityResources")}>
          <div className="extra-fields">
            <Field
              label={tr.t("manage.source")}
              value={tr.t("manage.resourceOfficial")}
              reason={tr.t("manage.mirrorUnavailable")}
            />
            <Field label={tr.t("manage.filename")}>
              <LauncherSelect
                label={tr.t("manage.filename")}
                value={preferenceEditor.prefs.management.download_file_name}
                options={[
                  { value: "original", label: tr.t("manage.keepFilename") },
                  {
                    value: "project_version",
                    label: tr.t("manage.projectFilename"),
                  },
                ]}
                disabled={preferenceEditor.disabled("save_policy")}
                reason={preferenceEditor.reason(
                  "save_policy",
                  tr.t("manage.filenameUnavailable"),
                )}
                onChange={(download_file_name) =>
                  preferenceEditor.patch("save_policy", {
                    management: { download_file_name },
                  })
                }
              />
            </Field>
            <Field label={tr.t("manage.modStyle")}>
              <LauncherSelect
                label={tr.t("manage.modStyle")}
                value={preferenceEditor.prefs.management.mod_display_style}
                options={[
                  { value: "metadata", label: tr.t("manage.localMetadata") },
                  { value: "file_name", label: tr.t("manage.fileNameStyle") },
                ]}
                disabled={preferenceEditor.disabled("mod_display_style")}
                reason={preferenceEditor.reason(
                  "mod_display_style",
                  tr.t("manage.modStyleUnavailable"),
                )}
                onChange={(mod_display_style) =>
                  preferenceEditor.patch("mod_display_style", {
                    management: { mod_display_style },
                  })
                }
              />
            </Field>
            <Field label={tr.t("manage.quick")}>
              <LauncherSelect
                label={tr.t("manage.quick")}
                value={preferenceEditor.prefs.management.quick_download}
                options={[
                  { value: "ask", label: tr.t("manage.ask") },
                  { value: "last_folder", label: tr.t("manage.lastFolder") },
                ]}
                disabled={preferenceEditor.disabled("save_policy")}
                reason={preferenceEditor.reason(
                  "save_policy",
                  tr.t("manage.quickUnavailable"),
                )}
                onChange={(quick_download) =>
                  preferenceEditor.patch("save_policy", {
                    management: { quick_download },
                  })
                }
              />
            </Field>
          </div>
          <p className="launcher-setting-summary">
            {tr.t("manage.savePolicyHelp")}
          </p>
          <Check checked reason={tr.t("manage.quiltUnavailable")}>
            {tr.t("manage.hideQuilt")}
          </Check>
          <Check checked reason={tr.t("manage.dependenciesUnavailable")}>
            {tr.t("manage.dependencies")}
          </Check>
        </Card>
        <Card title={tr.t("manage.assistance")}>
          <div className="extra-fields">
            <Field label={tr.t("manage.gameUpdates")}>
              <div className="extra-two extra-update-hints">
                <LauncherCheck
                  value={
                    preferenceEditor.prefs.management
                      .minecraft_release_notifications
                  }
                  disabled={preferenceEditor.disabled("minecraft_updates")}
                  reason={preferenceEditor.reason(
                    "minecraft_updates",
                    tr.t("manage.stableUnavailable"),
                  )}
                  onChange={(minecraft_release_notifications) =>
                    preferenceEditor.patch("minecraft_updates", {
                      management: { minecraft_release_notifications },
                    })
                  }
                >
                  {tr.t("manage.stable")}
                </LauncherCheck>
                <LauncherCheck
                  value={
                    preferenceEditor.prefs.management
                      .minecraft_snapshot_notifications
                  }
                  disabled={preferenceEditor.disabled("minecraft_updates")}
                  reason={preferenceEditor.reason(
                    "minecraft_updates",
                    tr.t("manage.betaUnavailable"),
                  )}
                  onChange={(minecraft_snapshot_notifications) =>
                    preferenceEditor.patch("minecraft_updates", {
                      management: { minecraft_snapshot_notifications },
                    })
                  }
                >
                  {tr.t("manage.beta")}
                </LauncherCheck>
              </div>
            </Field>
            <Field label={tr.t("manage.gameLanguage")}>
              <Check reason={tr.t("manage.gameLanguageUnavailable")}>
                {tr.t("manage.autoLanguage")}
              </Check>
            </Field>
          </div>
          <LauncherCheck
            value={
              preferenceEditor.prefs.management.clipboard_resource_detection
            }
            disabled={preferenceEditor.disabled("clipboard_detection")}
            reason={preferenceEditor.reason(
              "clipboard_detection",
              tr.t("manage.clipboardUnavailable"),
            )}
            onChange={(clipboard_resource_detection) =>
              preferenceEditor.patch("clipboard_detection", {
                management: { clipboard_resource_detection },
              })
            }
          >
            {tr.t("manage.clipboard")}
          </LauncherCheck>
          <p className="launcher-setting-summary">
            {tr.t("manage.clipboardHelp")}
          </p>
        </Card>
      </div>
    );
  if (section === "personalize")
    return (
      <LauncherPersonalize
        {...preferenceProps}
        api={api}
        revealHidden={revealHidden}
      />
    );
  if (section === "language")
    return <LauncherLanguage {...preferenceProps} api={api} />;
  if (section === "misc") return <LauncherMisc {...preferenceProps} />;
  if (section === "update")
    return (
      <div className="extra-settings extra-software-update">
        <Card>
          <div className="extra-fields">
            <Field label={tr.t("update.channel")}>
              <LauncherSelect
                label={tr.t("update.channel")}
                value={preferenceEditor.prefs.updates.channel}
                options={[
                  { value: "stable", label: tr.t("update.stable") },
                  { value: "beta", label: tr.t("update.beta") },
                ]}
                disabled={preferenceEditor.disabled("updates")}
                reason={preferenceEditor.reason(
                  "updates",
                  tr.t("update.channelUnavailable"),
                )}
                onChange={(channel) =>
                  preferenceEditor.patch("updates", { updates: { channel } })
                }
              />
            </Field>
            <Field label={tr.t("update.policy")}>
              <LauncherSelect
                label={tr.t("update.policy")}
                value={preferenceEditor.prefs.updates.policy}
                options={[
                  { value: "manual", label: tr.t("update.manual") },
                  { value: "notify", label: tr.t("update.notify") },
                  { value: "download", label: tr.t("update.download") },
                ]}
                disabled={preferenceEditor.disabled("updates")}
                reason={preferenceEditor.reason(
                  "updates",
                  tr.t("update.policyUnavailable"),
                )}
                onChange={(policy) =>
                  preferenceEditor.patch("updates", { updates: { policy } })
                }
              />
            </Field>
            <Field label={tr.t("update.source")}>
              <div className="ce-inline">
                <input
                  className="ce-field"
                  value={tr.t("update.sourceValue")}
                  readOnly
                  aria-label={tr.t("update.source")}
                />
                <LinkButton api={api} primary url={project + "/releases"}>
                  {tr.t("update.releases")}
                </LinkButton>
              </div>
            </Field>
          </div>
        </Card>
        <LauncherUpdates
          api={api}
          native={native}
          enabled={!!supportedEffects?.includes("updates")}
          busy={!!preferenceBusy}
          localization={preferenceEditor.prefs.localization}
          onNotify={onNotify}
        />
      </div>
    );
  if (section === "feedback")
    return (
      <div className="extra-settings">
        <Card title={tr.t("feedback.submit")}>
          <p className="extra-paragraph">
            {tr.t("feedback.help")}
            <br />
            {tr.t("feedback.listHelp")}
          </p>
          <div className="extra-actions">
            <LinkButton primary api={api} url={project + "/issues"}>
              {tr.t("feedback.open")}
            </LinkButton>
          </div>
        </Card>
        {(
          [
            "feedback.inProgress",
            "feedback.pending",
            "feedback.waiting",
          ] as const
        ).map((name, i) => (
          <Card key={name} title={tr.t(name)} collapse>
            {loading ? (
              <p className="extra-muted">{tr.t("feedback.loading")}</p>
            ) : feedbackError ? (
              <p className="extra-muted">
                {tr.t("feedback.unavailable")} {tr.serviceError(feedbackError)}
              </p>
            ) : feedback.filter((v) =>
                i === 0
                  ? v.labels.some((x) =>
                      ["in progress", "正在处理"].includes(x),
                    )
                  : i === 1
                    ? !v.labels.some((x) =>
                        ["in progress", "正在处理", "waiting", "等待"].includes(
                          x,
                        ),
                      )
                    : v.labels.some((x) => ["waiting", "等待"].includes(x)),
              ).length === 0 ? (
              <p className="extra-muted">{tr.t("feedback.empty")}</p>
            ) : (
              feedback
                .filter((v) =>
                  i === 0
                    ? v.labels.some((x) =>
                        ["in progress", "正在处理"].includes(x),
                      )
                    : i === 1
                      ? !v.labels.some((x) =>
                          [
                            "in progress",
                            "正在处理",
                            "waiting",
                            "等待",
                          ].includes(x),
                        )
                      : v.labels.some((x) => ["waiting", "等待"].includes(x)),
                )
                .map((v) => (
                  <button
                    className="extra-issue"
                    key={v.url}
                    onClick={() =>
                      void api("ui_open_link", { url: v.url }).catch((e) =>
                        onNotify(tr.serviceError(e)),
                      )
                    }
                  >
                    <FeedbackIcon active={i === 0} />
                    <div>
                      {v.title}
                      <small>
                        {v.labels.join("　")}　{v.author} |{" "}
                        {tr.formatDate(v.created_at)}
                      </small>
                    </div>
                  </button>
                ))
            )}
          </Card>
        ))}
      </div>
    );
  if (section === "logs")
    return (
      <LauncherLogs
        key={`${settings.root_id || ""}:${settings.root}`}
        api={rootApi || api}
        scopeKey={`${settings.root_id || ""}:${settings.root}`}
        initialLog={initialLog}
        native={native}
        operationsEnabled={!!supportedEffects?.includes("logs")}
        busy={!!preferenceBusy}
        language={preferenceEditor.prefs.localization.language}
        region={
          (preferences || defaultLauncherPreferenceView()).preferences
            .localization.region
        }
        onOpen={onOpen}
        onNotify={onNotify}
      />
    );
  if (section === "about")
    return (
      <div className="extra-settings">
        <Card title={tr.t("nav.about")}>
          <Credit
            name="龙腾猫跃"
            description={tr.t("about.originalAuthor")}
            action={tr.t("about.author")}
            api={api}
            url="https://github.com/LTCatt"
          />
          <Credit
            name="PCL Community"
            description={tr.t("about.community")}
            action={tr.t("about.github")}
            api={api}
            url="https://github.com/PCL-Community"
          />
          <Credit
            name="PCL RH"
            description={tr.t("about.version")}
            image={launcherIcon}
            action={tr.t("about.source")}
            api={api}
            url={project}
          />
        </Card>
        <Card title={tr.t("about.thanks")}>
          <Credit
            name="bangbang93"
            description={tr.t("about.bmclapi")}
            action={tr.t("about.author")}
            api={api}
            url="https://github.com/bangbang93"
          />
          <Credit
            name="MC 百科"
            description={tr.t("about.mcmod")}
            action={tr.t("about.openWiki")}
            api={api}
            url="https://www.mcmod.cn/"
          />
          <Credit
            name="Pysio @ Akaere Network"
            description={tr.t("about.cloud")}
            action={tr.t("about.contributor")}
            api={api}
            url="https://github.com/Pysio2007"
          />
          <Credit
            name="云默安 @ 至远光辉"
            description={tr.t("about.cloud")}
            action={tr.t("personalize.project")}
            api={api}
            url={project}
          />
          <Credit
            name="EasyTier"
            description={tr.t("about.easytier")}
            action={tr.t("about.project")}
            api={api}
            url="https://github.com/EasyTier"
          />
          <Credit name="z0z0r4" description={tr.t("about.mcim")} />
          <Credit name="Emperornummy" description={tr.t("about.icons")} />
        </Card>
        <Card title={tr.t("about.contributors")}>
          {contributorError && (
            <p className="extra-muted">
              {tr.t("about.contributorsUnavailable")}{" "}
              {tr.serviceError(contributorError)}
            </p>
          )}
          <div className="extra-contributors">
            {contributors.map((v) => (
              <button
                key={v.login}
                title={v.login}
                onClick={() =>
                  void api("ui_open_link", { url: v.url }).catch((e) =>
                    onNotify(tr.serviceError(e)),
                  )
                }
              >
                <span className="extra-contributor-monogram" aria-hidden="true">
                  {v.login.slice(0, 2)}
                </span>
                <span>{v.login}</span>
              </button>
            ))}
          </div>
          <div className="extra-more">
            <LinkButton
              api={api}
              url="https://github.com/PCL-Community/PCL-CE/graphs/contributors"
            >
              {tr.t("common.more")}
            </LinkButton>
          </div>
        </Card>
        <Card title={tr.t("about.legal")} collapse>
          <div className="extra-legal">
            <h3>{tr.t("about.privacy")}</h3>
            <p>{tr.t("about.privacyText")}</p>
            <h3>{tr.t("about.other")}</h3>
            <p>
              {tr.t("about.independent")}
              <br />
              {tr.t("about.minecraftNotice")}
            </p>
          </div>
          <div className="ce-actions extra-actions">
            <LinkButton api={api} url={project}>
              {tr.t("about.linuxSource")}
            </LinkButton>
            <LinkButton
              api={api}
              url={project + "/blob/master/docs/AUTHENTICATION.md"}
            >
              {tr.t("about.accounts")}
            </LinkButton>
            <LinkButton api={api} url={project + "/blob/master/docs/USAGE.md"}>
              {tr.t("about.guide")}
            </LinkButton>
          </div>
        </Card>
        <Card title={tr.t("about.attribution")} collapse>
          <div className="extra-legal">
            <h3>{tr.t("about.assetSources")}</h3>
            <p>{tr.t("about.attributionText")}</p>
            <p>{tr.t("about.noLobby")}</p>
          </div>
          <div className="ce-actions extra-actions">
            <LinkButton api={api} url="https://github.com/PCL-Community/PCL-CE">
              {tr.t("about.upstream")}
            </LinkButton>
            <LinkButton api={api} url={project + "/blob/master/COPYRIGHT.md"}>
              {tr.t("about.assetNotice")}
            </LinkButton>
          </div>
        </Card>
        <Card title={tr.t("about.licenses")} collapse>
          <LegalDocuments
            title={tr.t("about.offlineNotices")}
            loading={tr.t("about.documentLoading")}
            unavailable={tr.t("about.documentUnavailable")}
            retry={tr.t("about.documentRetry")}
          />
          {[
            {
              name: "Noto Sans CJK SC",
              copyright:
                "Copyright 2014–2021 Adobe. Noto is a Google trademark.",
              license: "SIL Open Font License 1.1",
              source: "https://github.com/notofonts/noto-cjk",
              document:
                project + "/blob/master/apps/desktop/src/assets/fonts/LICENSE",
            },
            {
              name: "PCL RH",
              copyright: "Copyright (c) 2026 PCL RH contributors",
              license: "MIT",
              source: project,
              document: project + "/blob/master/LICENSE",
            },
            {
              name: "Third-party dependencies",
              copyright: tr.t("about.thirdPartyRights"),
              license: tr.t("about.thirdPartyLicenses"),
              source: project + "/blob/master/THIRDPARTY.generated.json",
              document: project + "/blob/master/LICENSES/DEPENDENCIES.txt",
            },
            {
              name: "React",
              copyright: "Copyright © Meta Platforms, Inc. and affiliates.",
              license: "MIT",
              source: "https://github.com/facebook/react",
              document: "https://github.com/facebook/react/blob/main/LICENSE",
            },
            {
              name: "Tauri",
              copyright: "Copyright © Tauri contributors",
              license: "MIT / Apache-2.0",
              source: "https://github.com/tauri-apps/tauri",
              document:
                "https://github.com/tauri-apps/tauri/blob/dev/LICENSE_MIT",
            },
          ].map((v) => (
            <div className="extra-license" key={v.name}>
              <strong>{v.name}</strong>
              <div>
                {v.copyright}
                <br />
                {v.license}
                <div className="ce-actions extra-actions">
                  <LinkButton api={api} url={v.source}>
                    {tr.t("about.sourceSite")}
                  </LinkButton>
                  <LinkButton api={api} url={v.document}>
                    {tr.t("about.license")}
                  </LinkButton>
                </div>
              </div>
            </div>
          ))}
        </Card>
        <button
          className="extra-back-top"
          aria-label={tr.t("common.backTop")}
          onClick={() =>
            document
              .querySelector(".content")
              ?.scrollTo({ top: 0, behavior: "smooth" })
          }
        >
          <ArrowUp size={20} />
        </button>
      </div>
    );
  return null;
}
function Credit({
  name,
  description,
  image,
  action,
  api,
  url,
}: {
  name: string;
  description: string;
  image?: string;
  action?: string;
  api?: Api;
  url?: string;
}) {
  const tr = useContext(TranslationContext);
  const [failed, setFailed] = useState(false);
  return (
    <div className="extra-credit">
      {image && !failed ? (
        <img src={image} onError={() => setFailed(true)} alt="" />
      ) : (
        <span className="extra-credit-fallback">{name.slice(0, 2)}</span>
      )}
      <div>
        <span>{name}</span>
        <small>{description}</small>
      </div>
      {action &&
        (api && url ? (
          <LinkButton api={api} url={url}>
            {action}
          </LinkButton>
        ) : (
          <button
            className="ce-button"
            disabled
            title={tr.t("common.unavailable")}
          >
            {action}
          </button>
        ))}
    </div>
  );
}
type LogRow = {
  name: string;
  path: string;
  modified: number;
  current: boolean;
};
type Issue = {
  title: string;
  url: string;
  labels: string[];
  author: string;
  created_at: string;
};
type Contributor = { login: string; avatar: string; url: string };

function FeedbackIcon({ active }: { active: boolean }) {
  return active ? (
    <img className="extra-issue-mark" src={commandIcon} alt="" />
  ) : (
    <svg className="extra-issue-mark" viewBox="0 0 32 32" aria-hidden="true">
      <image
        href={lampTexture}
        width="16"
        height="16"
        transform="matrix(1,-.5,1,.5,0,8)"
      />
      <image
        href={lampTexture}
        width="16"
        height="16"
        transform="matrix(1,.5,0,1,0,8)"
      />
      <image
        href={lampTexture}
        width="16"
        height="16"
        transform="matrix(1,-.5,0,1,16,16)"
        style={{ filter: "brightness(.8)" }}
      />
    </svg>
  );
}
