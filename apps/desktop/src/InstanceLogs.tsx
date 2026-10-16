import { useEffect, useRef, useState } from "react";
import { Download, FileText, LoaderCircle, RotateCcw, X } from "lucide-react";
import { createTranslator } from "./i18n";
import { LauncherLogContent } from "./LauncherLogContent";
import type { LauncherLocale } from "./launcherTypes";
import type {
  InstanceLogContent,
  InstanceLogEntry,
  InstanceLogExportResponse,
  InstanceLogKind,
  InstanceLogListing,
} from "./instanceLogTypes";
import type { Api } from "./types";
import "./instance-logs.css";

export type InstanceLogsProps = {
  id: string;
  scopeKey: string;
  api: Api;
  native: boolean;
  disabled: boolean;
  revision?: string;
  language: LauncherLocale;
  region: LauncherLocale;
  onClose: () => void;
};
type Scope = Pick<
  InstanceLogsProps,
  "id" | "scopeKey" | "api" | "native" | "disabled" | "revision"
>;
type ListingState = {
  scope: Scope;
  status: "loading" | "ready" | "error";
  listing?: InstanceLogListing;
  error?: string;
};
type ReadingState = {
  scope: Scope;
  listing: InstanceLogListing;
  entry: InstanceLogEntry;
  serial: number;
  status: "loading" | "ready" | "error";
  content?: InstanceLogContent;
  error?: string;
};
type ListRequest = { scope: Scope };
type ReadRequest = { scope: Scope; listing: InstanceLogListing };
type ExportState = {
  reading: ReadingState;
  result?: InstanceLogExportResponse;
  error?: string;
};
const kinds: InstanceLogKind[] = ["game", "crash"];

/** A listing never chooses a file. Every explicit file click starts a new read
 * and search child, including rereads of identical text. Scope changes retire
 * requests during render; publication also retires saved row handlers before
 * React commits. Refresh removes the previous listing, selection and content.
 * Export handlers and notices belong to one successful read. An admitted native
 * save keeps its busy claim until completion, even after that read is retired.
 */
export function InstanceLogs({
  id,
  scopeKey,
  api,
  native,
  disabled,
  revision,
  language,
  region,
  onClose,
}: InstanceLogsProps) {
  const tr = createTranslator({ language, region });
  const owner = useRef<Scope>({
    id,
    scopeKey,
    api,
    native,
    disabled,
    revision,
  });
  const live = useRef(true);
  const closed = useRef(false);
  const listRequest = useRef<ListRequest | null>(null);
  const readRequest = useRef<ReadRequest | null>(null);
  const serial = useRef(0);
  const latestListing = useRef<ListingState | null>(null);
  const latestReading = useRef<ReadingState | null>(null);
  const exportRequest = useRef<object | null>(null);
  const [listingState, setListing] = useState<ListingState | null>(null);
  const [readingState, setReading] = useState<ReadingState | null>(null);
  const [exporting, setExporting] = useState(false);
  const [exportState, setExportState] = useState<ExportState | null>(null);
  const closeButton = useRef<HTMLButtonElement>(null);
  const callbacks = useRef({ onClose });
  callbacks.current = { onClose };
  if (
    owner.current.id !== id ||
    owner.current.scopeKey !== scopeKey ||
    owner.current.api !== api ||
    owner.current.native !== native ||
    owner.current.disabled !== disabled ||
    owner.current.revision !== revision
  ) {
    owner.current = { id, scopeKey, api, native, disabled, revision };
    listRequest.current = null;
    readRequest.current = null;
    latestListing.current = null;
    latestReading.current = null;
  }
  const scope = owner.current;
  const current = () =>
    live.current && !closed.current && owner.current === scope;
  const allowed = () => current() && scope.native && !scope.disabled;
  const publishListing = (next: ListingState | null) => {
    latestListing.current = next;
    setListing(next);
  };
  const publishReading = (next: ReadingState | null) => {
    latestReading.current = next;
    setReading(next);
    setExportState(null);
  };
  function close() {
    if (!current()) return;
    closed.current = true;
    listRequest.current = null;
    readRequest.current = null;
    publishListing(null);
    publishReading(null);
    callbacks.current.onClose();
  }
  async function refresh() {
    if (!allowed() || listRequest.current?.scope === scope) return;
    const request = { scope };
    listRequest.current = request;
    readRequest.current = null;
    publishReading(null);
    publishListing({ scope, status: "loading" });
    const owns = () => allowed() && listRequest.current === request;
    try {
      const listing = await scope.api<InstanceLogListing>("instance_logs", {
        id: scope.id,
      });
      if (!owns()) return;
      if (listing.id !== scope.id)
        throw new Error(tr.t("instanceLogs.wrongInstance"));
      publishListing({ scope, status: "ready", listing });
    } catch (error) {
      if (owns())
        publishListing({
          scope,
          status: "error",
          error: tr.serviceError(error),
        });
    } finally {
      if (listRequest.current === request) listRequest.current = null;
    }
  }
  async function read(entry: InstanceLogEntry, source: ListingState) {
    if (
      !allowed() ||
      latestListing.current !== source ||
      source.status !== "ready" ||
      !source.listing ||
      !source.listing.entries.includes(entry)
    )
      return;
    const listing = source.listing;
    const request = { scope, listing };
    readRequest.current = request;
    const selection = { scope, listing, entry, serial: ++serial.current };
    publishReading({ ...selection, status: "loading" });
    const owns = () =>
      allowed() &&
      readRequest.current === request &&
      latestListing.current === source;
    try {
      const content = await scope.api<InstanceLogContent>("instance_read_log", {
        id: scope.id,
        kind: entry.kind,
        name: entry.name,
        revision: entry.revision,
        listingRevision: listing.revision,
      });
      if (!owns()) return;
      if (
        content.id !== scope.id ||
        content.kind !== entry.kind ||
        content.name !== entry.name
      )
        throw new Error(tr.t("instanceLogs.wrongFile"));
      publishReading({ ...selection, status: "ready", content });
    } catch (error) {
      if (owns())
        publishReading({
          ...selection,
          status: "error",
          error: tr.serviceError(error),
        });
    } finally {
      if (readRequest.current === request) readRequest.current = null;
    }
  }
  function exportable(
    source: ReadingState,
  ): source is ReadingState & { content: InstanceLogContent } {
    return (
      allowed() &&
      latestReading.current === source &&
      latestListing.current?.listing === source.listing &&
      source.status === "ready" &&
      !!source.content?.text &&
      typeof source.content.content_revision === "string" &&
      source.content.content_revision !== ""
    );
  }
  async function exportText(source: ReadingState) {
    if (!exportable(source) || exportRequest.current) return;
    const request = {};
    exportRequest.current = request;
    setExporting(true);
    setExportState(null);
    const owns = () => exportable(source) && exportRequest.current === request;
    try {
      const result = await scope.api<InstanceLogExportResponse>(
        "instance_export_log",
        {
          id: scope.id,
          kind: source.entry.kind,
          name: source.entry.name,
          revision: source.entry.revision,
          listingRevision: source.listing.revision,
          contentRevision: source.content.content_revision,
        },
      );
      if (owns() && result.status !== "cancelled")
        setExportState({ reading: source, result });
    } catch (error) {
      if (owns())
        setExportState({ reading: source, error: tr.serviceError(error) });
    } finally {
      if (exportRequest.current === request) {
        exportRequest.current = null;
        if (live.current) setExporting(false);
      }
    }
  }
  useEffect(() => {
    live.current = true;
    closeButton.current?.focus();
    return () => {
      live.current = false;
      listRequest.current = null;
      readRequest.current = null;
      latestListing.current = null;
      latestReading.current = null;
    };
  }, []);
  useEffect(() => {
    void refresh();
    return () => {
      if (listRequest.current?.scope === scope) listRequest.current = null;
      if (readRequest.current?.scope === scope) readRequest.current = null;
    };
  }, [scope]);

  const readable = native && !disabled && !closed.current;
  const displayed = listingState?.scope === scope ? listingState : null;
  const listing = readable ? displayed?.listing : undefined;
  const reading =
    readable &&
    readingState?.scope === scope &&
    readingState.listing === listing
      ? readingState
      : null;
  const loading = readable && (!displayed || displayed.status === "loading");
  const exportNotice = exportState?.reading === reading ? exportState : null;
  const size = (bytes: number) =>
    bytes < 1024
      ? `${tr.formatNumber(bytes)} B`
      : bytes < 1024 * 1024
        ? `${tr.formatNumber(bytes / 1024, { maximumFractionDigits: 1 })} KiB`
        : `${tr.formatNumber(bytes / (1024 * 1024), { maximumFractionDigits: 1 })} MiB`;

  return (
    <div
      className="modal-shade ce-instance-logs-shade"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) close();
      }}
    >
      <section
        className="ce-instance-logs-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="instance-logs-title"
        aria-busy={loading || reading?.status === "loading" || exporting}
        tabIndex={-1}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            close();
          }
          if (event.key !== "Tab") return;
          event.stopPropagation();
          const controls = [
            ...event.currentTarget.querySelectorAll<HTMLElement>(
              "button:not(:disabled),input:not(:disabled)",
            ),
          ];
          const first = controls[0],
            last = controls[controls.length - 1];
          if (!first) {
            event.preventDefault();
            event.currentTarget.focus();
          } else if (
            !controls.includes(document.activeElement as HTMLElement)
          ) {
            event.preventDefault();
            (event.shiftKey ? last : first)?.focus();
          } else if (event.shiftKey && document.activeElement === first) {
            event.preventDefault();
            last?.focus();
          } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault();
            first.focus();
          }
        }}
      >
        <header className="ce-instance-logs-heading">
          <FileText size={22} aria-hidden="true" />
          <h2 id="instance-logs-title">
            {tr.t("instanceLogs.title", { name: id })}
          </h2>
          <button
            className="icon-button"
            type="button"
            aria-label={tr.t("instanceLogs.close")}
            onClick={close}
          >
            <X size={18} aria-hidden="true" />
          </button>
        </header>
        {!readable ? (
          <p className="ce-instance-logs-state" role="status">
            {tr.t(
              native ? "instanceLogs.disabled" : "instanceLogs.desktopOnly",
            )}
          </p>
        ) : loading ? (
          <p className="ce-instance-logs-state" role="status">
            <LoaderCircle className="spin" size={19} aria-hidden="true" />
            {tr.t("instanceLogs.loading")}
          </p>
        ) : displayed?.status === "error" ? (
          <div className="ce-instance-logs-error" role="alert">
            <strong>{tr.t("instanceLogs.listFailed")}</strong>
            <p>{displayed.error}</p>
          </div>
        ) : listing ? (
          <>
            <div className="ce-instance-logs-directory">
              <span title={listing.game_dir}>{listing.game_dir}</span>
              {listing.shared && <small>{tr.t("instanceLogs.shared")}</small>}
            </div>
            <div className="ce-instance-logs-workspace">
              <div className="ce-instance-logs-files">
                <div className="ce-instance-logs-file-list">
                  {listing.entries.length === 0 ? (
                    <p className="ce-instance-logs-placeholder" role="status">
                      {tr.t("instanceLogs.empty")}
                    </p>
                  ) : (
                    kinds.map((kind) => {
                      const entries = listing.entries.filter(
                        (entry) => entry.kind === kind,
                      );
                      if (!entries.length) return null;
                      return (
                        <section key={kind} className="ce-instance-logs-group">
                          <h3>{tr.t(`instanceLogs.${kind}`)}</h3>
                          {entries.map((entry) => (
                            <button
                              key={`${entry.kind}:${entry.name}`}
                              type="button"
                              className="ce-instance-logs-file"
                              aria-pressed={reading?.entry === entry}
                              onClick={() => {
                                if (displayed) void read(entry, displayed);
                              }}
                            >
                              <strong>{entry.name}</strong>
                              <span>
                                <time>
                                  {entry.modified === null
                                    ? "—"
                                    : tr.formatDate(entry.modified * 1000, {
                                        dateStyle: "short",
                                        timeStyle: "short",
                                      })}
                                </time>
                                <small>{size(entry.size)}</small>
                              </span>
                            </button>
                          ))}
                        </section>
                      );
                    })
                  )}
                </div>
                {listing.truncated && (
                  <p className="ce-instance-logs-note" role="status">
                    {tr.t("instanceLogs.listTruncated")}
                  </p>
                )}
              </div>
              <section
                className="ce-instance-logs-reader"
                aria-label={tr.t("instanceLogs.content")}
              >
                {reading ? (
                  <>
                    <div className="ce-instance-logs-reader-heading">
                      <h3 className="ce-instance-logs-selected">
                        {reading.entry.name}
                      </h3>
                      <button
                        className="ce-button ce-instance-logs-export"
                        type="button"
                        disabled={exporting || !exportable(reading)}
                        onClick={() => void exportText(reading)}
                      >
                        {exporting ? (
                          <LoaderCircle
                            className="spin"
                            size={15}
                            aria-hidden="true"
                          />
                        ) : (
                          <Download size={15} aria-hidden="true" />
                        )}
                        {tr.t(
                          exporting
                            ? "instanceLogs.exporting"
                            : "instanceLogs.export",
                        )}
                      </button>
                    </div>
                    {reading.status === "loading" ? (
                      <p className="ce-instance-logs-state" role="status">
                        <LoaderCircle
                          className="spin"
                          size={18}
                          aria-hidden="true"
                        />
                        {tr.t("instanceLogs.reading")}
                      </p>
                    ) : reading.status === "error" ? (
                      <div className="ce-instance-logs-error" role="alert">
                        <strong>{tr.t("instanceLogs.readFailed")}</strong>
                        <p>{reading.error}</p>
                      </div>
                    ) : reading.content ? (
                      <>
                        {exportNotice && (
                          <p
                            className={`ce-instance-logs-note ce-instance-logs-export-notice${exportNotice.error ? " is-error" : ""}`}
                            role={exportNotice.error ? "alert" : "status"}
                          >
                            <span
                              title={exportNotice.result?.path ?? undefined}
                            >
                              {tr.t(
                                exportNotice.error
                                  ? "instanceLogs.exportFailed"
                                  : exportNotice.result?.status === "complete"
                                    ? "instanceLogs.exported"
                                    : "instanceLogs.exportUnavailable",
                              )}
                            </span>
                            {exportNotice.error && (
                              <span>{exportNotice.error}</span>
                            )}
                            {exportNotice.result?.warning && (
                              <span>{exportNotice.result.warning}</span>
                            )}
                          </p>
                        )}
                        {reading.content.truncated && (
                          <p className="ce-instance-logs-note" role="status">
                            {tr.t("instanceLogs.textTruncated")}
                          </p>
                        )}
                        {reading.content.text === "" && (
                          <p className="ce-instance-logs-note" role="status">
                            {tr.t("instanceLogs.emptyFile")}
                          </p>
                        )}
                        <LauncherLogContent
                          key={reading.serial}
                          text={reading.content.text}
                          language={language}
                          region={region}
                        />
                      </>
                    ) : null}
                  </>
                ) : (
                  <p className="ce-instance-logs-placeholder" role="status">
                    {tr.t("instanceLogs.selectFile")}
                  </p>
                )}
              </section>
            </div>
          </>
        ) : null}
        <footer className="ce-instance-logs-footer">
          <button
            className="ce-button primary"
            type="button"
            disabled={!readable || loading}
            onClick={() => void refresh()}
          >
            <RotateCcw size={16} aria-hidden="true" />
            {tr.t("instanceLogs.refresh")}
          </button>
          <button
            ref={closeButton}
            className="ce-button"
            type="button"
            onClick={close}
          >
            {tr.t("instanceLogs.close")}
          </button>
        </footer>
      </section>
    </div>
  );
}
