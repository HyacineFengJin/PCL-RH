import type { ComponentType } from "react";
/** View IDs are declared statically or within their bundled owner's namespace. */
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
  contextNote: string;
  contextPaths: string[];
};
export type ConversationContext = Pick<
  Conversation,
  "contextNote" | "contextPaths"
>;
export type Target = { minecraft: string; loaders: string[] };
export type Project = {
  id: string;
  name: string;
  folder: string;
  updatedAt: number;
  targets: Target[];
  conversations: Conversation[];
  layout: Layout | null;
  instructions: string;
  /** Older projects have no process grant. Changes apply to future captures. */
  buildPolicy?: { enabled: boolean; network: boolean };
};
export type PluginScope = "launcher" | "rh-code" | "both";
/** Host surfaces that are currently alive; `both` is a manifest lifetime. */
export type ActivePluginScope = Exclude<PluginScope, "both">;
export type Manifest = {
  /** Native interfaces implemented by the same product plugin, not UI cards. */
  nativeServices?: { id: string; version: number }[];
  /** Omitted legacy entries conservatively retain the Node runtime. */
  runtimeNeeds?: ("native" | "node")[];
  id: string;
  name: string;
  version: string;
  required: boolean;
  /** Omitted in older manifests: RH Code only. */
  scope?: PluginScope;
  dependencies: string[];
  capabilities: string[];
  views: ViewId[];
  /** Trusted bundled code may publish a bounded set of views in this namespace. */
  dynamicViews?: { prefix: string; limit: number };
  commands?: string[];
  layouts?: string[];
  /** Native-captured grants are resolved by the trusted Node tool catalog. */
  tools?: { name: string; capability: string; modes: Mode[] }[];
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
  /** Keep the RH Code controller alive for unsaved local state or background
   * work. Release when finished; the view host releases remaining mount leases
   * when this view unmounts. */
  keepAwake(): () => void;
  invoke<T>(command: string, ...args: unknown[]): T;
};
export type ViewProps = { area: Area; api: ViewHost };
export type ViewContribution = {
  id: ViewId;
  owner: string;
  label: string;
  icon: ComponentType<{ size?: number }>;
  /** Semantic color follows a component when users move it between areas. */
  tone?: "neutral" | "blue" | "purple" | "orange";
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
  state: "active" | "dormant" | "disabled" | "blocked" | "failed" | "stopped";
  error?: string;
  dependencies?: string[];
};
export type FileBuffer = {
  content: string;
  original: string;
  sha256: string | null;
};
