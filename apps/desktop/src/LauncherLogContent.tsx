import { useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import { createTranslator } from "./i18n";
import type { LauncherLocale } from "./launcherTypes";
import {
  boundLogSearchQuery,
  LOG_SEARCH_MATCH_LIMIT,
  LOG_SEARCH_QUERY_LIMIT,
  nextLogMatch,
  searchLogText,
} from "./launcherLogSearch";
import "./launcher-log-content.css";

type Props = {
  text: string;
  language: LauncherLocale;
  region: LauncherLocale;
};
type SearchState = {
  text: string;
  query: string;
  caseSensitive: boolean;
  current: number;
  queryTruncated: boolean;
};
const emptySearch = (text: string): SearchState => ({
  text,
  query: "",
  caseSensitive: false,
  current: 0,
  queryTruncated: false,
});

/** This child owns local search only: it has no API, file or network access.
 * LauncherLogs remounts it for each accepted read, even identical text. The
 * text owner also retires old search/handlers immediately if props are replaced.
 * Match navigation reuses the bounded scan and text slices; the DOM holds one
 * copy of the log and at most 1,000 marks, never one log copy per result.
 */
export function LauncherLogContent({ text, language, region }: Props) {
  const tr = createTranslator({ language, region });
  const [stored, setSearch] = useState(() => emptySearch(text));
  const search = stored.text === text ? stored : emptySearch(text);
  const currentOwner = useRef(search);
  currentOwner.current = search;
  const live = useRef(true);
  const searchPanel = useRef<HTMLDivElement>(null);
  const activeMark = useRef<HTMLElement>(null);
  const logViewport = useRef<HTMLPreElement>(null);
  const result = useMemo(
    () => searchLogText(text, search.query, search.caseSensitive),
    [text, search.query, search.caseSensitive],
  );
  const segments = useMemo(() => {
    const parts: string[] = [];
    let start = 0;
    for (const match of result.matches) {
      parts.push(
        text.slice(start, match.start),
        text.slice(match.start, match.end),
      );
      start = match.end;
    }
    parts.push(text.slice(start));
    return parts;
  }, [text, result]);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  useEffect(() => {
    const viewport = logViewport.current,
      mark = activeMark.current;
    if (!viewport || !mark) return;
    // A pre can extend below the visible main even when its own match is in
    // view. Keep the fitted search panel visible in its owning main first;
    // scrollIntoView would also move unrelated ancestors or the window.
    const panel = searchPanel.current;
    const main = panel?.closest<HTMLElement>("main.content");
    let mainRange: { top: number; bottom: number } | undefined;
    if (panel && main) {
      const mainBounds = main.getBoundingClientRect();
      const mainTop = mainBounds.top + main.clientTop + 8;
      const mainBottom = mainTop + main.clientHeight - 16;
      mainRange = { top: mainTop, bottom: mainBottom };
      const panelBounds = panel.getBoundingClientRect();
      if (
        panelBounds.top < mainTop ||
        panelBounds.height > mainBottom - mainTop
      )
        main.scrollTop += panelBounds.top - mainTop;
      else if (panelBounds.bottom > mainBottom)
        main.scrollTop += panelBounds.bottom - mainBottom;
    }
    // Restrict positioning to the part of the pre actually visible in main.
    // This also handles an unusually short viewport or a clamped main scroll.
    // Neither stage changes focus, window scroll or another scroll container.
    const bounds = viewport.getBoundingClientRect();
    const target = mark.getBoundingClientRect();
    const top = Math.max(
      bounds.top + viewport.clientTop,
      mainRange?.top ?? -Infinity,
    );
    const bottom = Math.min(
      bounds.top + viewport.clientTop + viewport.clientHeight,
      mainRange?.bottom ?? Infinity,
    );
    if (bottom <= top) return;
    if (target.top < top || target.height > bottom - top)
      viewport.scrollTop += target.top - top;
    else if (target.bottom > bottom)
      viewport.scrollTop += target.bottom - bottom;
  }, [result, search]);
  const active = () => live.current && currentOwner.current === search;
  function updateQuery(value: string) {
    if (!active()) return;
    const query = boundLogSearchQuery(value);
    setSearch({
      ...search,
      query,
      current: 0,
      queryTruncated: query !== value,
    });
  }
  function move(direction: -1 | 1) {
    if (!active() || !result.matches.length) return;
    setSearch({
      ...search,
      current: nextLogMatch(search.current, direction, result.matches.length),
    });
  }
  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.nativeEvent.isComposing || !active()) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      updateQuery("");
    }
  }
  const count = result.matches.length;
  const summary = !search.query
    ? tr.t("logs.searchIdle")
    : !count
      ? tr.t("logs.searchNoMatches")
      : tr.t("logs.searchPosition", {
          current: tr.formatNumber(search.current + 1),
          count: tr.formatNumber(count),
        });
  return (
    <div
      className="launcher-log-content"
      ref={searchPanel}
      onKeyDown={onKeyDown}
    >
      <div className="launcher-log-search" role="search">
        <label className="launcher-log-query">
          <span>{tr.t("logs.searchLabel")}</span>
          <input
            className="ce-field"
            type="text"
            value={search.query}
            maxLength={LOG_SEARCH_QUERY_LIMIT}
            placeholder={tr.t("logs.searchPlaceholder")}
            title={tr.t("logs.searchScope", {
              limit: tr.formatNumber(LOG_SEARCH_QUERY_LIMIT),
            })}
            autoComplete="off"
            spellCheck={false}
            onChange={(event) => updateQuery(event.currentTarget.value)}
            onPaste={(event) => {
              if (!active()) return;
              const field = event.currentTarget;
              const start = field.selectionStart ?? field.value.length;
              const end = field.selectionEnd ?? start;
              const next =
                field.value.slice(0, start) +
                event.clipboardData.getData("text") +
                field.value.slice(end);
              // Native maxlength would truncate before onChange, losing the
              // evidence needed to tell the user that a paste was shortened.
              if (next.length > LOG_SEARCH_QUERY_LIMIT) {
                event.preventDefault();
                updateQuery(next);
              }
            }}
            onKeyDown={(event) => {
              if (
                event.key !== "Enter" ||
                event.nativeEvent.isComposing ||
                event.altKey ||
                event.ctrlKey ||
                event.metaKey
              )
                return;
              event.preventDefault();
              event.stopPropagation();
              move(event.shiftKey ? -1 : 1);
            }}
          />
        </label>
        <label className="ce-check launcher-log-case">
          <input
            type="checkbox"
            checked={search.caseSensitive}
            onChange={(event) => {
              if (active())
                setSearch({
                  ...search,
                  caseSensitive: event.currentTarget.checked,
                  current: 0,
                });
            }}
          />
          <span>{tr.t("logs.searchCaseSensitive")}</span>
        </label>
        <div className="launcher-log-search-actions">
          <button
            type="button"
            className="ce-button"
            disabled={!count}
            title={tr.t("logs.searchPreviousShortcut")}
            onClick={() => move(-1)}
          >
            {tr.t("logs.searchPrevious")}
          </button>
          <button
            type="button"
            className="ce-button"
            disabled={!count}
            title={tr.t("logs.searchNextShortcut")}
            onClick={() => move(1)}
          >
            {tr.t("logs.searchNext")}
          </button>
          <button
            type="button"
            className="ce-button"
            disabled={!search.query}
            title={tr.t("logs.searchClearShortcut")}
            onClick={() => updateQuery("")}
          >
            {tr.t("logs.searchClear")}
          </button>
        </div>
      </div>
      <div
        className="launcher-log-search-status"
        role="status"
        aria-live="polite"
      >
        {summary}
        {result.truncated && (
          <span>
            {tr.t("logs.searchMatchLimit", {
              limit: tr.formatNumber(LOG_SEARCH_MATCH_LIMIT),
              minimum: tr.formatNumber(LOG_SEARCH_MATCH_LIMIT + 1),
            })}
          </span>
        )}
        {search.queryTruncated && (
          <span>
            {tr.t("logs.searchQueryTruncated", {
              limit: tr.formatNumber(LOG_SEARCH_QUERY_LIMIT),
            })}
          </span>
        )}
      </div>
      <pre className="log-view launcher-log-text" ref={logViewport}>
        {segments.map((part, index) =>
          index % 2 ? (
            <mark
              key={index}
              className={
                (index - 1) / 2 === search.current ? "is-current" : undefined
              }
              aria-current={
                (index - 1) / 2 === search.current ? "true" : undefined
              }
              ref={(index - 1) / 2 === search.current ? activeMark : undefined}
            >
              {part}
            </mark>
          ) : (
            part
          ),
        )}
      </pre>
    </div>
  );
}
