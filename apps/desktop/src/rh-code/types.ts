import type { ComponentType } from "react";
/** View IDs are declared by their owning bundled plugin manifest. */
export type ViewId = string;
export type JsonValue =
  null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
export type Area = {
  type: "area";
  id: string;
  view: ViewId;
  conversation?: string;
  file?: string;
  /** Per-view data stays with the area when its selected view changes. */
  state?: Record<ViewId, Record<string, JsonValue>>;
};
export type Split = {
  type: "split";
  id: string;
  axis: "row" | "column";
  ratio: number;
  children: [Layout, Layout];
};
export type Layout = Area | Split;
export type Mode = "ask" | "plan" | "execute";
export type Message = {
  id: string;
  role: "user" | "assistant" | "notice";
  content: string;
  jobId: string | null;
};
export type Conversation = {
  id: string;
  title: string;
  draft: string;
  mode: Mode;
  messages: Message[];
  activeJob: string | null;
};
export type Target = { minecraft: string; loaders: string[] };
export type Project = {
  id: string;
  name: string;
  folder: string;
  updatedAt: number;
  targets: Target[];
  conversations: Conversation[];
  layout: Layout | null;
};
export type Manifest = {
  id: string;
  name: string;
  version: string;
  required: boolean;
  dependencies: string[];
  capabilities: string[];
  views: ViewId[];
  commands?: string[];
  layouts?: string[];
};
export type LayoutPreset = { id: string; name: string; layout: Layout };
export type Snapshot = {
  revision: string;
  projects: Project[];
  disabledPlugins: string[];
  plugins: Manifest[];
  layoutPresets: LayoutPreset[];
};
/** Views use this host to avoid depending on the workspace controller. State
 * replacement affects only this view's key in the current area. */
export type ViewHost = {
  project: Project | null;
  state: Record<string, JsonValue>;
  updateState(next: Record<string, JsonValue>): void;
  invoke<T>(command: string, ...args: unknown[]): T;
};
export type ViewProps = { area: Area; api: ViewHost };
export type ViewContribution = {
  id: ViewId;
  owner: string;
  label: string;
  icon: ComponentType<{ size?: number }>;
  render: ComponentType<ViewProps>;
  settings?: ComponentType<ViewProps>;
};
export type LayoutContribution = {
  id: string;
  owner: string;
  label: string;
  create(project: Project): Layout;
};
export type CommandContribution = {
  id: string;
  owner: string;
  handler: (...args: any[]) => unknown;
};
export type PluginStatus = {
  id: string;
  state: "active" | "disabled" | "blocked" | "failed" | "stopped";
  error?: string;
  dependencies?: string[];
};
export type FileBuffer = { content: string; original: string; sha256: string };
