/** Search only the redacted text already owned by the log panel. Offsets refer
 * to the original UTF-16 string, including Unicode case folding; never search
 * a lowercased copy whose length may differ. Results are non-overlapping and
 * stop after one extra match proves that the navigation/highlight cap applies.
 */
export const LOG_SEARCH_QUERY_LIMIT = 256;
export const LOG_SEARCH_MATCH_LIMIT = 1000;

export type LogTextMatch = { start: number; end: number };
export type LogTextSearch = {
  matches: LogTextMatch[];
  truncated: boolean;
};

export function boundLogSearchQuery(value: string): string {
  let end = Math.min(value.length, LOG_SEARCH_QUERY_LIMIT);
  // Do not turn a supplementary character crossing the limit into a lone half.
  if (
    end < value.length &&
    value.charCodeAt(end - 1) >= 0xd800 &&
    value.charCodeAt(end - 1) <= 0xdbff &&
    value.charCodeAt(end) >= 0xdc00 &&
    value.charCodeAt(end) <= 0xdfff
  )
    --end;
  return value.slice(0, end);
}

export function searchLogText(
  text: string,
  input: string,
  caseSensitive: boolean,
): LogTextSearch {
  const query = boundLogSearchQuery(input);
  const matches: LogTextMatch[] = [];
  if (!query) return { matches, truncated: false };
  // The pattern has only escaped literal text: no user regex syntax, backtracks
  // from quantifiers, or whole-log transformed copy. Unicode matching preserves
  // source indices (e.g. dotted I, final sigma, Kelvin sign and astral letters).
  const literal = query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const pattern = new RegExp(literal, caseSensitive ? "gu" : "giu");
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(text)) !== null) {
    if (matches.length === LOG_SEARCH_MATCH_LIMIT)
      return { matches, truncated: true };
    matches.push({ start: match.index, end: pattern.lastIndex });
  }
  return { matches, truncated: false };
}

export function nextLogMatch(
  current: number,
  direction: -1 | 1,
  count: number,
): number {
  return count ? (current + direction + count) % count : 0;
}
