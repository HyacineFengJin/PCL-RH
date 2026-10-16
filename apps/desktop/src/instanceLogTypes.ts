/** Read-only instance logs use separate list and file revisions. A row is only
 * readable against the exact listing that supplied it; the native service owns
 * directory/file identity checks, gzip decoding and bounded, redacted text. */
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
};
