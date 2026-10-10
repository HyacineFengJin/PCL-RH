import type { ComponentType } from "react";
export type ViewId =
  | "chat"
  | "threads"
  | "graph"
  | "files"
  | "source"
  | "targets"
  | "changes"
  | "logs"
  | "porting"
  | "legacy";
export type Area = {
  type: "area";
  id: string;
  view: ViewId;
  conversation?: string;
  file?: string;
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
};
export type Snapshot = {
  revision: string;
  projects: Project[];
  disabledPlugins: string[];
  plugins: Manifest[];
};
export type ViewContribution = {
  id: ViewId;
  owner: string;
  label: string;
  icon: ComponentType<{ size?: number }>;
  render: ComponentType<{ area: Area }>;
};
export type FileBuffer = { content: string; original: string; sha256: string };
