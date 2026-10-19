import { ExperimentalVersion } from "./ExperimentalVersion";
import { useEffect, useRef, useState } from "react";
import type { Api } from "./types";
import { t, serviceError, type MessageKey } from "./i18n";
import { InstanceOperationDialog } from "./instanceOperationUi";
import "./extensionCompatibility.css";
import {
  notifyExtensionsChanged,
  subscribeExtensionsChanged,
} from "./extensionEvents";
import {
  useExtensionRequestScope,
  useExtensionView,
  type ExtensionRequest,
} from "./extensionRequestScope";
import {
  experimentalCall as call,
  type ExtensionEntry,
  type ExtensionReview,
  type ExtensionCompatibilityReport,
  type ExtensionCard,
  type ExtensionScope,
  type ExperimentalNavigate,
} from "./experimentalTypes";
const capabilityLabel = (id: string) =>
  t(
    (
      {
        "ui.cards": "experimental.capCards",
        "launcher.navigate": "experimental.capNavigate",
        "instances.summary": "experimental.capSummary",
      } as Record<string, MessageKey>
    )[id],
  );
const scopeLabel = (scope: ExtensionScope = "launcher") =>
  t(`experimental.communityScope.${scope}`);
const slotLabel = (slot: string) => {
  const labels: Record<string, MessageKey> = {
    "home.secondary": "experimental.communitySlot.home",
    "tools.cards": "experimental.communitySlot.tools",
    "rh-code.cards": "experimental.communitySlot.code",
  };
  return labels[slot] ? t(labels[slot]) : slot;
};
type ScopeFilter = "all" | "launcher" | "rh-code";
const matchesScope = (entry: ExtensionEntry, filter: ScopeFilter) =>
  filter === "all" ||
  entry.scope === "both" ||
  (entry.scope ?? "launcher") === filter;
const expectedEntry = (entry: ExtensionEntry) => ({
  digest: entry.digest,
  generation: entry.generation,
});
type ExtensionMutation =
  | "extensions_enable"
  | "extensions_disable"
  | "extensions_revoke"
  | "extensions_remove"
  | "extensions_confirm"
  | "extensions_safe_mode";

// Retirement does not depend on a successful cleanup RPC. The host's review
// TTL is the fallback if an old API is already unavailable.
function cancelReview(api: Api, token: string) {
  try {
    void call(api, "extensions_cancel", { token }).catch(() => {});
  } catch {}
}

/** Manifest titles and bodies are React text only. No plugin receives our Api,
 * DOM, settings or runtime object; card actions consume a native typed intent. */
export function ExperimentalCards({
  api,
  native,
  slot,
  onNavigate,
  emptyText,
}: {
  api: Api;
  native: boolean;
  slot: "tools.cards" | "home.secondary" | "rh-code.cards";
  onNavigate: ExperimentalNavigate;
  emptyText?: string;
}) {
  type Summary = {
    minecraftVersion: string;
    loader: string;
    modCount: number;
    isolated: boolean;
  };
  const [epoch, setEpoch] = useState(0);
  const scope = useExtensionRequestScope(api, native, `${slot}:${epoch}`);
  const view = useExtensionView<{
    cards: ExtensionCard[];
    summary: Summary | null;
    error: string;
    busy: boolean;
    loaded: boolean;
  }>(scope.owner, {
    cards: [],
    summary: null,
    error: "",
    busy: false,
    loaded: false,
  });
  const { cards, summary, error, busy, loaded } = view.value;
  const navigate = useRef(onNavigate);
  navigate.current = onNavigate;
  useEffect(
    () =>
      subscribeExtensionsChanged(() => {
        // Retire requests and visible actions synchronously, before React renders
        // the next owner. A late intent cannot navigate during that gap.
        scope.retireOperation();
        view.update({
          cards: [],
          summary: null,
          busy: false,
          error: "",
          loaded: false,
        });
        setEpoch((value) => value + 1);
      }),
    [scope.owner],
  );
  useEffect(() => {
    const request = scope.beginRead();
    if (request) {
      view.update({
        cards: [],
        summary: null,
        error: "",
        busy: false,
        loaded: false,
      });
      void call<ExtensionCard[]>(api, "extensions_cards", { slot })
        .then((v) => {
          if (request.current()) view.update({ cards: v, loaded: true });
        })
        .catch((e) => {
          if (request.current()) view.update({ error: serviceError(e) });
        })
        .finally(request.finish);
    }
    return () => request?.finish();
  }, [scope.owner]);
  async function act(card: ExtensionCard, actionId: string) {
    if (
      !scope.current() ||
      !view.current()?.cards.includes(card) ||
      !card.actions.some((a) => a.id === actionId && a.enabled)
    )
      return;
    const request = scope.beginOperation();
    if (!request) return;
    view.update({ busy: true, error: "" });
    try {
      const intent = await call<{
        kind: string;
        target: Parameters<ExperimentalNavigate>[0];
        summary: Summary;
      }>(api, "extensions_action", {
        extensionId: card.extensionId,
        cardId: card.id,
        actionId,
        expected: { digest: card.digest, generation: card.generation },
      });
      if (!request.current() || !view.current()?.cards.includes(card)) return;
      if (intent.kind === "navigate") navigate.current(intent.target);
      else if (intent.kind === "show-instance-summary")
        view.update({ summary: intent.summary });
    } catch (e) {
      if (request.current()) view.update({ error: serviceError(e) });
    } finally {
      if (request.current()) view.update({ busy: false });
      request.finish();
    }
  }
  return (
    <>
      {emptyText && loaded && cards.length === 0 && !error && (
        <p className="experimental-origin">{emptyText}</p>
      )}
      {cards.map((card) => (
        <section
          key={`${card.extensionId}:${card.id}`}
          className="ce-card experimental-card"
        >
          <h2 className="ce-card-title">{card.title}</h2>
          <p className="experimental-origin">{card.extensionName}</p>
          <p className="experimental-body">{card.text}</p>
          <div className="ce-actions">
            {card.actions.map((a) => (
              <button
                key={a.id}
                className="ce-button"
                disabled={!a.enabled || busy || !native}
                onClick={() => void act(card, a.id)}
              >
                {a.label}
              </button>
            ))}
          </div>
        </section>
      ))}
      {error && (
        <p className="experimental-error" role="alert">
          {error}
        </p>
      )}
      {summary && (
        <section className="ce-card">
          <h2 className="ce-card-title">{t("experimental.summary")}</h2>
          <dl className="experimental-summary">
            <dt>{t("experimental.version")}</dt>
            <dd>{summary.minecraftVersion}</dd>
            <dt>{t("experimental.loader")}</dt>
            <dd>{summary.loader}</dd>
            <dt>{t("experimental.mods")}</dt>
            <dd>{summary.modCount}</dd>
            <dt>{t("experimental.isolated")}</dt>
            <dd>
              {t(summary.isolated ? "experimental.yes" : "experimental.no")}
            </dd>
          </dl>
          <button
            className="ce-button"
            onClick={() => {
              if (scope.current() && view.current()?.summary === summary)
                view.update({ summary: null });
            }}
          >
            {t("common.cancel")}
          </button>
        </section>
      )}
    </>
  );
}

export function ExperimentalExtensions({
  api,
  native,
  onNavigate,
}: {
  api: Api;
  native: boolean;
  onNavigate: ExperimentalNavigate;
}) {
  type Dialog = {
    owner: object;
    lifetime: object;
    api: Api;
    value: ExtensionReview | ExtensionCompatibilityReport;
    grants: string[];
    valid: boolean;
    released: boolean;
    committing: boolean;
  };
  type Removal = {
    owner: object;
    lifetime: object;
    entry: ExtensionEntry;
    valid: boolean;
    committing: boolean;
  };
  const scope = useExtensionRequestScope(api, native, "extensions-manager");
  const view = useExtensionView<{
    entries: ExtensionEntry[];
    dialog: Dialog | null;
    removal: Removal | null;
    filter: ScopeFilter;
    safeMode: boolean;
    busy: boolean;
    error: string;
    cardsRevision: number;
  }>(scope.owner, {
    entries: [],
    dialog: null,
    removal: null,
    filter: "all",
    safeMode: false,
    busy: false,
    error: "",
    cardsRevision: 0,
  });
  const { entries, dialog, removal, filter, safeMode, busy, error } =
    view.value;
  const visibleEntries = entries.filter((entry) => matchesScope(entry, filter));
  const review = dialog && !("kind" in dialog.value) ? dialog.value : null;
  const compatibility = dialog && "kind" in dialog.value ? dialog.value : null;
  const grants = dialog?.grants ?? [];
  const activeDialog = useRef<Dialog | null>(null);
  const activeRemoval = useRef<Removal | null>(null);

  const ownsDialog = (value: Dialog) =>
    value.valid &&
    activeDialog.current === value &&
    value.owner === scope.owner &&
    scope.ownsLifetime(value.lifetime);
  function retireDialog(value: Dialog, release = true) {
    value.valid = false;
    if (activeDialog.current === value) activeDialog.current = null;
    // Only pending consent is cancelled. Once confirm was submitted, the host
    // owns its transaction; renderer retirement is not an uninstall or rollback.
    if (
      release &&
      !value.released &&
      !value.committing &&
      !("kind" in value.value)
    ) {
      value.released = true;
      cancelReview(value.api, value.value.token);
    }
  }
  function retireRemoval(value: Removal) {
    value.valid = false;
    if (activeRemoval.current === value) activeRemoval.current = null;
  }
  const ownsRemoval = (value: Removal) =>
    value.valid &&
    activeRemoval.current === value &&
    value.owner === scope.owner &&
    scope.ownsLifetime(value.lifetime);
  async function refresh(parent?: ExtensionRequest) {
    if (parent && !parent.current()) return;
    const request = scope.beginRead();
    if (!request) return;
    try {
      const result = await call<{
        entries: ExtensionEntry[];
        safeMode: boolean;
        warning: string | null;
      }>(api, "extensions_list");
      if (request.current() && (!parent || parent.current()))
        view.update({
          entries: result.entries,
          safeMode: result.safeMode,
          error: result.warning || "",
        });
      return request.current() && (!parent || parent.current());
    } catch (e) {
      if (request.current() && (!parent || parent.current()))
        view.update({ error: serviceError(e) });
    } finally {
      request.finish();
    }
  }
  useEffect(() => {
    if (scope.current()) {
      view.update({ dialog: null, removal: null, busy: false });
      void refresh();
    }
    return () => {
      const pending = activeDialog.current;
      if (pending?.owner === scope.owner) retireDialog(pending);
      const removing = activeRemoval.current;
      if (removing?.owner === scope.owner) retireRemoval(removing);
    };
  }, [scope.owner]);
  async function perform(
    operation: (request: ExtensionRequest) => Promise<unknown>,
    refreshAfter = true,
  ) {
    // Ref admission closes the same-render double-click window before awaiting
    // or relying on React's rendered busy flag.
    const request = scope.beginOperation();
    if (!request) return;
    view.update({ busy: true, error: "" });
    try {
      await operation(request);
      if (request.current() && refreshAfter) await refresh(request);
    } catch (e) {
      if (request.current()) view.update({ error: serviceError(e) });
    } finally {
      if (request.current()) view.update({ busy: false });
      request.finish();
    }
  }
  async function mutate<T = unknown>(
    operation: ExtensionMutation,
    args: Record<string, unknown>,
  ) {
    const result = await call<T>(api, operation, args);
    // Publication can succeed after this page retires; every mounted surface
    // still needs to drop actions based on the previously published state.
    notifyExtensionsChanged();
    return result;
  }
  function acceptReview(
    value: ExtensionReview | ExtensionCompatibilityReport,
    request: ExtensionRequest,
  ) {
    if (!request.current()) {
      // The submitted read may already have created a token. Release it through
      // its captured API, even though no follow-up workflow may use that API.
      if (!("kind" in value)) cancelReview(api, value.token);
      return;
    }
    if (activeDialog.current) retireDialog(activeDialog.current);
    if (activeRemoval.current) retireRemoval(activeRemoval.current);
    const next: Dialog = {
      owner: scope.owner,
      lifetime: request.lifetime,
      api,
      value,
      grants:
        "kind" in value
          ? []
          : value.capabilities
              .filter((c) => c.currentlyGranted)
              .map((c) => c.id),
      valid: true,
      released: false,
      committing: false,
    };
    activeDialog.current = next;
    view.update({ dialog: next, removal: null });
  }
  function dismiss(value: Dialog) {
    if (!ownsDialog(value) || value.committing) return;
    scope.retireOperation();
    retireDialog(value);
    view.update({ dialog: null, busy: false });
  }
  async function importReview(request: ExtensionRequest) {
    const picked = await api<{
      status: string;
      path?: string;
      message?: string;
    }>("experimental_choose", { kind: "extension" });
    if (!request.current()) return;
    if (picked.status === "selected")
      acceptReview(
        await call<ExtensionReview | ExtensionCompatibilityReport>(
          api,
          "extensions_review",
          { path: picked.path },
        ),
        request,
      );
    else if (picked.status === "unavailable") throw new Error(picked.message);
  }
  function performForEntry(
    entry: ExtensionEntry,
    operation: (request: ExtensionRequest) => Promise<unknown>,
    refreshAfter = true,
  ) {
    const current = view.current();
    if (
      scope.current() &&
      current?.entries.includes(entry) &&
      matchesScope(entry, current.filter)
    )
      void perform(operation, refreshAfter);
  }
  async function reviewEntry(entry: ExtensionEntry, request: ExtensionRequest) {
    acceptReview(
      await call<ExtensionReview>(api, "extensions_rereview", { id: entry.id }),
      request,
    );
  }
  function enableEntry(entry: ExtensionEntry) {
    performForEntry(
      entry,
      async (request) => {
        if (entry.state === "suspended") {
          await reviewEntry(entry, request);
          return;
        }
        const result = await mutate<ExtensionEntry>("extensions_enable", {
          id: entry.id,
          expected: expectedEntry(entry),
        });
        await refresh(request);
        if (request.current() && result.state === "suspended")
          await reviewEntry(result, request);
      },
      false,
    );
  }
  function requestRemoval(entry: ExtensionEntry) {
    const current = view.current();
    if (
      !scope.current() ||
      scope.busy() ||
      !current?.entries.includes(entry) ||
      !matchesScope(entry, current.filter)
    )
      return;
    if (activeDialog.current) retireDialog(activeDialog.current);
    if (activeRemoval.current) retireRemoval(activeRemoval.current);
    const next: Removal = {
      owner: scope.owner,
      lifetime: scope.owner.lifetime!,
      entry,
      valid: true,
      committing: false,
    };
    activeRemoval.current = next;
    view.update({ dialog: null, removal: next });
  }
  function dismissRemoval(value: Removal) {
    if (!ownsRemoval(value) || value.committing) return;
    scope.retireOperation();
    retireRemoval(value);
    view.update({ removal: null, busy: false });
  }
  function confirmRemoval(value: Removal) {
    if (
      !ownsRemoval(value) ||
      value.committing ||
      !view.current()?.entries.includes(value.entry)
    )
      return;
    void perform(async (request) => {
      if (!request.current() || !ownsRemoval(value)) return;
      value.committing = true;
      view.update({ removal: value });
      try {
        await mutate("extensions_remove", {
          id: value.entry.id,
          expected: expectedEntry(value.entry),
        });
      } finally {
        retireRemoval(value);
        if (request.current()) view.update({ removal: null });
      }
    });
  }
  function confirm(value: Dialog) {
    if (
      !ownsDialog(value) ||
      value.committing ||
      "kind" in value.value ||
      value.value.capabilities.some(
        (c) => c.required && !value.grants.includes(c.id),
      )
    )
      return;
    const token = value.value.token;
    void perform(async (request) => {
      if (!request.current() || !ownsDialog(value)) return;
      value.committing = true;
      view.update({ dialog: value });
      try {
        await mutate("extensions_confirm", {
          token,
          grants: [...value.grants],
        });
      } finally {
        // A consumed confirmation cannot be revived by saved callbacks, even
        // on failure. A fresh review is required for another consent attempt.
        retireDialog(value, false);
        if (request.current()) view.update({ dialog: null });
      }
    });
  }
  return (
    <>
      <section className="ce-card">
        <h2 className="ce-card-title">{t("experimental.communityTitle")}</h2>
        <p>{t("experimental.communityHelp")}</p>
        <div className="ce-actions">
          <button
            className="ce-button"
            disabled={busy || !native}
            onClick={() => void perform(importReview, false)}
          >
            {t("experimental.importExtension")}
          </button>
          <button
            className="ce-button"
            disabled={busy || !native}
            onClick={() =>
              void perform(async (request) => {
                if (await refresh(request))
                  view.update({
                    cardsRevision: (view.current()?.cardsRevision ?? 0) + 1,
                  });
              }, false)
            }
          >
            {t("experimental.refresh")}
          </button>
        </div>
        <label className="ce-check experimental-safe">
          <input
            type="checkbox"
            checked={safeMode}
            disabled={busy || !native}
            onChange={(e) => {
              const enabled = e.target.checked;
              void perform(() => mutate("extensions_safe_mode", { enabled }));
            }}
          />
          <span>{t("experimental.safeMode")}</span>
        </label>
        <ExperimentalVersion version="0.7.0" />
      </section>
      {error && (
        <p className="experimental-error" role="alert">
          {error}
        </p>
      )}
      <div
        className="ce-actions extension-scope-filter"
        role="group"
        aria-label={t("experimental.communityScopeFilter")}
      >
        {(["all", "launcher", "rh-code"] as const).map((value) => (
          <button
            key={value}
            className={filter === value ? "ce-button primary" : "ce-button"}
            aria-pressed={filter === value}
            disabled={busy || !native}
            onClick={() => {
              if (!scope.current() || scope.busy()) return;
              if (activeRemoval.current) retireRemoval(activeRemoval.current);
              view.update({ filter: value, removal: null });
            }}
          >
            {value === "all"
              ? t("experimental.communityScopeAll")
              : scopeLabel(value)}
          </button>
        ))}
      </div>
      {visibleEntries.length === 0 && (
        <section className="ce-card">
          <p>
            {t(
              entries.length === 0
                ? "experimental.noExtensions"
                : "experimental.communityNoMatches",
            )}
          </p>
        </section>
      )}
      {visibleEntries.map((entry) => (
        <section className="ce-card" key={entry.id}>
          <h2 className="ce-card-title">
            {entry.name}{" "}
            <span className="experimental-origin">
              {entry.version} ·{" "}
              {t(`experimental.state.${entry.state}` as MessageKey)}
            </span>
          </h2>
          <p className="experimental-origin">
            {t("experimental.publisher")}: {entry.publisher}
          </p>
          <p className="experimental-origin extension-scope-label">
            {scopeLabel(entry.scope)}
            {(entry.slots?.length ?? 0) > 0 && (
              <span> · {entry.slots.map(slotLabel).join(" · ")}</span>
            )}
          </p>
          <div className="ce-actions">
            <button
              className="ce-button"
              disabled={busy || !native}
              onClick={() =>
                performForEntry(
                  entry,
                  (request) => reviewEntry(entry, request),
                  false,
                )
              }
            >
              {t("experimental.communityReviewPermissions")}
            </button>
            {(!(entry.enabled ?? entry.state !== "disabled") ||
              entry.state === "suspended") && (
              <button
                className="ce-button"
                disabled={busy || !native}
                onClick={() => enableEntry(entry)}
              >
                {t(
                  entry.state === "suspended"
                    ? "experimental.communityReauthorize"
                    : "experimental.communityEnable",
                )}
              </button>
            )}
            <button
              className="ce-button"
              disabled={
                busy ||
                !native ||
                !(entry.enabled ?? entry.state !== "disabled")
              }
              onClick={() =>
                performForEntry(entry, () =>
                  mutate("extensions_disable", {
                    id: entry.id,
                    expected: expectedEntry(entry),
                  }),
                )
              }
            >
              {t("experimental.disable")}
            </button>
            <button
              className="ce-button"
              disabled={busy || !native}
              onClick={() => requestRemoval(entry)}
            >
              {t("experimental.communityRemove")}
            </button>
          </div>
          <details className="extension-permissions">
            <summary>
              {t("experimental.permissions")} ({entry.grants.length})
            </summary>
            {entry.grants.length === 0 && (
              <p className="experimental-origin">
                {t("experimental.communityNoGrants")}
              </p>
            )}
            {entry.grants.map((cap) => (
              <div className="experimental-permission" key={cap}>
                <span>{capabilityLabel(cap)}</span>
                <button
                  className="ce-button"
                  disabled={busy || !native}
                  onClick={() =>
                    performForEntry(entry, () =>
                      mutate("extensions_revoke", {
                        id: entry.id,
                        capability: cap,
                        expected: expectedEntry(entry),
                      }),
                    )
                  }
                >
                  {t("experimental.revoke")}
                </button>
              </div>
            ))}
          </details>
        </section>
      ))}
      <ExperimentalCards
        key={view.value.cardsRevision}
        api={api}
        native={native}
        slot="tools.cards"
        onNavigate={onNavigate}
      />
      {removal && (
        <InstanceOperationDialog
          title={t("experimental.communityRemoveTitle")}
          titleId="experimental-extension-remove-title"
          busy={busy}
          committing={removal.committing}
          confirmLabel={t("experimental.communityRemove")}
          confirmDisabled={
            !native || !ownsRemoval(removal) || !entries.includes(removal.entry)
          }
          onClose={() => dismissRemoval(removal)}
          onConfirm={() => confirmRemoval(removal)}
        >
          <p>
            {t("experimental.communityRemoveConfirm", {
              name: removal.entry.name,
            })}
          </p>
        </InstanceOperationDialog>
      )}
      {compatibility && dialog && (
        <ExtensionCompatibilityDialog
          report={compatibility}
          onClose={() => dismiss(dialog)}
        />
      )}
      {review && dialog && (
        <InstanceOperationDialog
          title={t("experimental.permissions")}
          titleId="experimental-permission-title"
          busy={busy}
          committing={dialog.committing}
          confirmLabel={t("experimental.grant")}
          confirmDisabled={
            !native ||
            review.capabilities.some(
              (c) => c.required && !grants.includes(c.id),
            )
          }
          onClose={() => dismiss(dialog)}
          onConfirm={() => confirm(dialog)}
        >
          <h3>
            {review.name} · {review.version}
          </h3>
          <p>
            {t("experimental.publisher")}: {review.publisher}
          </p>
          <p className="experimental-origin">
            {t("experimental.communityReviewScope", {
              scope: scopeLabel(review.scope),
            })}
            {(review.slots?.length ?? 0) > 0 && (
              <span> · {review.slots.map(slotLabel).join(" · ")}</span>
            )}
          </p>
          {review.capabilities.map((cap) => (
            <label className="ce-check experimental-capability" key={cap.id}>
              <input
                type="checkbox"
                checked={grants.includes(cap.id)}
                disabled={busy}
                onChange={(e) => {
                  if (!ownsDialog(dialog) || dialog.committing || scope.busy())
                    return;
                  dialog.grants = e.target.checked
                    ? [...new Set([...dialog.grants, cap.id])]
                    : dialog.grants.filter((v) => v !== cap.id);
                  view.update({ dialog });
                }}
              />
              <span>
                <strong>
                  {capabilityLabel(cap.id)} ·{" "}
                  {t(
                    cap.required
                      ? "experimental.required"
                      : "experimental.optional",
                  )}
                </strong>
                <small>{cap.reason}</small>
              </span>
            </label>
          ))}
          <p className="experimental-origin experimental-digest">
            SHA-256: {review.digest}
          </p>
        </InstanceOperationDialog>
      )}
    </>
  );
}

/** Descriptors are text only; the report has no grants or install token. The
 * shared confirmation dialog keeps CE keyboard, focus and animation behavior. */
export function ExtensionCompatibilityDialog({
  report,
  onClose,
}: {
  report: ExtensionCompatibilityReport;
  onClose: () => void;
}) {
  const requirementKind = (required: boolean) =>
    t(required ? "experimental.required" : "experimental.optional");
  return (
    <InstanceOperationDialog
      title={t("experimental.compatTitle")}
      titleId="experimental-compatibility-title"
      busy={false}
      committing={false}
      confirmLabel={t("experimental.compatCannotInstall")}
      cancelLabel={t("experimental.compatClose")}
      confirmDisabled={true}
      onConfirm={() => {}}
      onClose={onClose}
    >
      <div className="extension-compatibility">
        <h3>
          {report.name} · {report.version}
        </h3>
        <p>{t("experimental.compatReadOnly")}</p>
        <dl className="experimental-summary">
          <dt>{t("experimental.compatEcosystem")}</dt>
          <dd>{report.ecosystem === "pcl-n" ? "PCL N" : "PCL Nex"}</dd>
          <dt>ID</dt>
          <dd>{report.id}</dd>
          <dt>{t("experimental.publisher")}</dt>
          <dd>{report.publisher ?? t("experimental.compatUndeclared")}</dd>
          <dt>{t("experimental.compatEntry")}</dt>
          <dd>{report.entryAssembly}</dd>
          {report.apiRange && (
            <>
              <dt>
                {t(
                  report.ecosystem === "pcl-n"
                    ? "experimental.compatApi"
                    : "experimental.compatCore",
                )}
              </dt>
              <dd>{report.apiRange}</dd>
            </>
          )}
        </dl>
        {report.archive && (
          <>
            <h3>
              {t("experimental.compatArchive")} · .{report.archive.format}
            </h3>
            <p>{t("experimental.compatArchiveUnverified")}</p>
            <dl className="experimental-summary">
              <dt>{t("experimental.compatArchiveSize")}</dt>
              <dd>{report.archive.bytes}</dd>
              <dt>{t("experimental.compatArchiveEntries")}</dt>
              <dd>{report.archive.entryCount}</dd>
              <dt>{t("experimental.compatArchiveDeclaredSize")}</dt>
              <dd>{report.archive.declaredUncompressedBytes}</dd>
            </dl>
          </>
        )}
        <ul>
          {report.findings.map((code) => (
            <li key={code}>{t(`experimental.compat.${code}`)}</li>
          ))}
        </ul>
        {report.requirements.length > 0 && (
          <>
            <h3>{t("experimental.compatServices")}</h3>
            <ul>
              {report.requirements.map((service) => (
                <li key={service.id}>
                  {service.id} · {service.range} ·{" "}
                  {requirementKind(service.required)} ·{" "}
                  {t(
                    service.coverage === "probe-only"
                      ? "experimental.compatProbeOnly"
                      : "experimental.compatUnavailable",
                  )}
                </li>
              ))}
            </ul>
          </>
        )}
        {report.permissions.length > 0 && (
          <>
            <h3>{t("experimental.permissions")}</h3>
            <ul>
              {report.permissions.map((permission) => (
                <li key={permission.id}>
                  {permission.id} · {requirementKind(permission.required)}
                  <p>{permission.reason}</p>
                </li>
              ))}
            </ul>
          </>
        )}
        {report.dependencies.length > 0 && (
          <>
            <h3>{t("experimental.compatDependencies")}</h3>
            <ul>
              {report.dependencies.map((dependency) => (
                <li key={dependency.id}>
                  {dependency.id} · {dependency.range} ·{" "}
                  {requirementKind(dependency.required)}
                </li>
              ))}
            </ul>
          </>
        )}
        {report.mixinConfigs.length > 0 && (
          <>
            <h3>{t("experimental.compatMixin")}</h3>
            <ul>
              {report.mixinConfigs.map((config) => (
                <li key={config}>{config}</li>
              ))}
            </ul>
          </>
        )}
        {report.platformDeclarations.length > 0 && (
          <>
            <h3>{t("experimental.compatPlatforms")}</h3>
            <ul>
              {report.platformDeclarations.map((platform) => (
                <li key={platform}>{platform}</li>
              ))}
            </ul>
          </>
        )}
        <p className="experimental-origin experimental-digest">
          {t("experimental.compatManifestDigest")}: {report.digest}
        </p>
      </div>
    </InstanceOperationDialog>
  );
}
