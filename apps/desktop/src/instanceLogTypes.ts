/** Instance logs bind listings, files and viewed text separately. A row is only
 * readable against its original listing; native owns identity checks, gzip
 * decoding and bounded, redacted text. Export sends that text's receipt. */
export type InstanceLogKind = "game" | "crash";
export type InstanceLogEntry = {
  kind: InstanceLogKind;
  name: string;
  size: number;
  /** Unix seconds, or null when the timestamp is unavailable. */
  modified: number | null;
  revision: string;
};
export type InstanceLogListing = {
  id: string;
  game_dir: string;
  shared: boolean;
  revision: string;
  entries: InstanceLogEntry[];
  truncated: boolean;
};
export type InstanceLogContent = {
  id: string;
  kind: InstanceLogKind;
  name: string;
  text: string;
  truncated: boolean;
  /** Native receipt for this exact bounded, redacted text. */
  content_revision: string;
};
export type InstanceLogExportResponse = {
  status: "cancelled" | "complete" | "unavailable";
  path: string | null;
  count: number;
  warning: string | null;
};
