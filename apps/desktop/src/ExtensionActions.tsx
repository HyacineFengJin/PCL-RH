/** Search the native catalog and submit only the exact published action. Search
 * and metadata refresh never execute actions or resolve unsaved card drafts. */
import { useEffect, useRef, useState } from "react";
import type { Api } from "./types";
import { t, serviceError } from "./i18n";
import {
  experimentalCall,
  type ExtensionCatalog,
  type ExtensionCatalogAction,
  type ExtensionInstanceSummary,
  type ExperimentalNavigate,
} from "./experimentalTypes";
import {
  notifyExtensionsChanged,
  notifyLauncherAppearanceApplied,
  subscribeExtensionsChanged,
} from "./extensionEvents";
import {
  useExtensionRequestScope,
  useExtensionView,
} from "./extensionRequestScope";

const matches = (action: ExtensionCatalogAction, query: string) => {
  const words = query.trim().toLocaleLowerCase().split(/\s+/u).filter(Boolean);
  const source = [
    action.label,
    action.extensionName,
    action.extensionId,
    action.cardTitle,
    action.cardId,
  ]
    .join(" ")
    .toLocaleLowerCase();
  return words.every((word) => source.includes(word));
};
export function ExtensionActions({
  api,
  native,
  onNavigate,
}: {
  api: Api;
  native: boolean;
  onNavigate: ExperimentalNavigate;
}) {
  const [epoch, setEpoch] = useState(0),
    [query, setQuery] = useState("");
  const search = useRef({ value: "", epoch: 0 }),
    navigate = useRef(onNavigate);
  navigate.current = onNavigate;
  const scope = useExtensionRequestScope(
    api,
    native,
    `extension-actions:${epoch}`,
  );
  const view = useExtensionView<{
    actions: ExtensionCatalogAction[];
    loaded: boolean;
    busy: boolean;
    error: string;
    feedback: string;
    summary: ExtensionInstanceSummary | null;
  }>(scope.owner, {
    actions: [],
    loaded: false,
    busy: false,
    error: "",
    feedback: "",
    summary: null,
  });
  useEffect(
    () =>
      subscribeExtensionsChanged(() => {
        scope.retireOperation();
        view.update({
          actions: [],
          loaded: false,
          busy: false,
          error: "",
          feedback: "",
          summary: null,
        });
        setEpoch((value) => value + 1);
      }),
    [scope.owner],
  );
  useEffect(() => {
    const request = scope.beginRead();
    if (request) {
      view.update({
        actions: [],
        loaded: false,
        busy: false,
        error: "",
        feedback: "",
        summary: null,
      });
      void experimentalCall<ExtensionCatalog>(api, "extensions_catalog")
        .then((catalog) => {
          if (request.current())
            view.update({ actions: catalog.actions, loaded: true });
        })
        .catch((error) => {
          if (request.current()) view.update({ error: serviceError(error) });
        })
        .finally(request.finish);
    }
    return () => request?.finish();
  }, [scope.owner]);
  const listed = view.value.actions.filter((action) => matches(action, query)),
    searchEpoch = search.current.epoch,
    summary = view.value.summary,
    lifetime = scope.owner.lifetime;
  const ownsPage = () => lifetime !== null && scope.ownsLifetime(lifetime);
  function currentAction(action: ExtensionCatalogAction, queryEpoch: number) {
    return (
      ownsPage() &&
      search.current.epoch === queryEpoch &&
      !!view.current()?.actions.includes(action) &&
      matches(action, search.current.value)
    );
  }
  async function act(action: ExtensionCatalogAction, queryEpoch: number) {
    if (!action.enabled || !currentAction(action, queryEpoch)) return;
    const request = scope.beginOperation();
    if (!request) return;
    view.update({ busy: true, error: "", feedback: "", summary: null });
    try {
      const intent = await experimentalCall<{
        kind: string;
        target: Parameters<ExperimentalNavigate>[0];
        summary: ExtensionInstanceSummary;
      }>(api, "extensions_action", {
        extensionId: action.extensionId,
        cardId: action.cardId,
        actionId: action.id,
        expected: { digest: action.digest, generation: action.generation },
      });
      // A committed native appearance change must reach preferences even if
      // its originating page/search has already retired.
      if (intent.kind === "launcher-appearance-applied")
        notifyLauncherAppearanceApplied();
      if (!request.current() || !currentAction(action, queryEpoch)) return;
      if (intent.kind === "navigate") navigate.current(intent.target);
      else if (intent.kind === "show-instance-summary")
        view.update({ summary: intent.summary });
      else if (
        intent.kind === "text-copied" ||
        intent.kind === "launcher-appearance-applied"
      )
        view.update({
          feedback: t(
            intent.kind === "text-copied"
              ? "experimental.textCopied"
              : "experimental.appearanceApplied",
          ),
        });
    } catch (error) {
      if (request.current()) view.update({ error: serviceError(error) });
    } finally {
      if (request.current()) view.update({ busy: false });
      request.finish();
    }
  }
  return (
    <div className="extension-action-catalog">
      <p className="experimental-origin">
        {t("experimental.savedActionsHelp")}
      </p>
      <div className="ce-actions">
        <button
          className="ce-button"
          disabled={!native}
          onClick={() => {
            if (ownsPage()) notifyExtensionsChanged();
          }}
        >
          {t("experimental.refreshPanels")}
        </button>
      </div>
      <label className="extension-action-search">
        <span>{t("experimental.actionsSearch")}</span>
        <input
          className="ce-field"
          type="search"
          value={query}
          disabled={!native}
          placeholder={t("experimental.actionsSearchPlaceholder")}
          title={t("experimental.actionsEnterHelp")}
          onChange={(event) => {
            if (!ownsPage()) return;
            search.current = {
              value: event.target.value,
              epoch: search.current.epoch + 1,
            };
            scope.retireOperation();
            view.update({ busy: false, feedback: "", summary: null });
            setQuery(event.target.value);
          }}
          onKeyDown={(event) => {
            if (event.key !== "Enter" || event.nativeEvent.isComposing) return;
            event.preventDefault();
            if (listed.length === 1) void act(listed[0], searchEpoch);
          }}
        />
      </label>
      <div className="extension-action-results">
        {listed.map((action) => (
          <section
            className="ce-card extension-action-row"
            key={JSON.stringify([action.extensionId, action.cardId, action.id])}
          >
            <div className="extension-action-context">
              <h2 className="ce-card-title">{action.label}</h2>
              <p className="experimental-origin">
                {action.extensionName === action.cardTitle
                  ? action.extensionName
                  : `${action.extensionName} · ${action.cardTitle}`}
              </p>
              {!action.enabled && (
                <p className="experimental-origin">
                  {t(
                    action.unavailableReason === "no-selected-instance"
                      ? "experimental.actionNoInstance"
                      : "experimental.actionUnavailable",
                  )}
                </p>
              )}
            </div>
            <button
              className="ce-button"
              disabled={!native || view.value.busy || !action.enabled}
              onClick={() => void act(action, searchEpoch)}
            >
              {t("experimental.actionRun")}
            </button>
          </section>
        ))}
      </div>
      {view.value.loaded && !listed.length && (
        <p className="experimental-origin" role="status">
          {t(
            view.value.actions.length
              ? "experimental.actionsNoMatch"
              : "experimental.actionsEmpty",
          )}
        </p>
      )}
      {view.value.error && (
        <p className="experimental-error" role="alert">
          {view.value.error}
        </p>
      )}
      {view.value.feedback && (
        <p className="experimental-origin" role="status">
          {view.value.feedback}
        </p>
      )}
      {view.value.summary && (
        <section className="ce-card">
          <h2 className="ce-card-title">{t("experimental.summary")}</h2>
          <dl className="experimental-summary">
            <dt>{t("experimental.version")}</dt>
            <dd>{view.value.summary.minecraftVersion}</dd>
            <dt>{t("experimental.loader")}</dt>
            <dd>{view.value.summary.loader}</dd>
            <dt>{t("experimental.mods")}</dt>
            <dd>{view.value.summary.modCount}</dd>
            <dt>{t("experimental.isolated")}</dt>
            <dd>
              {t(
                view.value.summary.isolated
                  ? "experimental.yes"
                  : "experimental.no",
              )}
            </dd>
          </dl>
          <button
            className="ce-button"
            onClick={() => {
              if (ownsPage() && view.current()?.summary === summary)
                view.update({ summary: null });
            }}
          >
            {t("common.cancel")}
          </button>
        </section>
      )}
    </div>
  );
}
