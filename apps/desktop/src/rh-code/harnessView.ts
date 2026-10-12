/** Read-only Harness projections shared by the conversation and review views.
 * The host owns proposal validation and application; displayed text never
 * becomes write authority. Applying uses only the host's proposal ID/digest. */
import type { JobView } from "../experimentalTypes";
import type { Conversation, Mode, Project } from "./types";
import { label } from "./locale";

export type HarnessChange = {
  path: string;
  kind: "create" | "modify" | "delete";
  beforeSha256: string | null;
  afterSha256: string | null;
  before: string | null;
  after: string | null;
  beforeTruncated?: boolean;
  afterTruncated?: boolean;
};
export type HarnessProposal = {
  id: string;
  digest: string;
  status: string;
  summary: string;
  changes: HarnessChange[];
  changesTruncated?: boolean;
  changeCount?: number;
};
export type HarnessProposalSet = {
  proposals: HarnessProposal[];
  total?: number;
  truncated?: boolean;
};

export const isActiveJob = (status: string) =>
  status === "queued" || status === "running";

export function jobStatusLabel(status: string) {
  const names: Record<string, [string, string]> = {
    queued: ["等待运行", "Queued"],
    running: ["运行中", "Running"],
    completed: ["已完成", "Completed"],
    cancelled: ["已停止", "Stopped"],
    failed: ["失败", "Failed"],
    interrupted: ["已中断", "Interrupted"],
    pending: ["待审阅", "Pending review"],
    applied: ["已应用", "Applied"],
  };
  return names[status] ? label(...names[status]) : status;
}

export function modeDescription(mode: Mode) {
  return mode === "execute"
    ? label("生成改动建议，审阅后应用", "Propose changes for review")
    : mode === "plan"
      ? label("只读分析与规划", "Read-only analysis and planning")
      : label("只读询问", "Read-only questions");
}

export function jobText(job: JobView) {
  // No raw provider state or reasoning is rendered. A host may project bounded
  // assistant output while running; the final public result takes precedence.
  const result = job.result;
  if (result && typeof result === "object" && "text" in result)
    if (typeof result.text === "string") return result.text;
  const output = (job as JobView & { output?: string }).output;
  return typeof output === "string" ? output : "";
}

export function conversationJobIds(conversations: readonly Conversation[]) {
  const ids = conversations.flatMap((conversation) => [
    ...conversation.messages.flatMap((message) =>
      message.jobId ? [message.jobId] : [],
    ),
    ...(conversation.activeJob ? [conversation.activeJob] : []),
  ]);
  return [...new Set(ids)];
}

export function projectJobOwner(project: Project, jobId: string) {
  return project.conversations.find(
    (conversation) =>
      conversation.activeJob === jobId ||
      conversation.messages.some((message) => message.jobId === jobId),
  );
}

export function newestJob(a: JobView | undefined, b: JobView | undefined) {
  if (!a) return b;
  if (!b) return a;
  return b.summary.revision >= a.summary.revision ? b : a;
}

export type DiffLine = {
  kind: "context" | "remove" | "add";
  text: string;
  noNewline: boolean;
};
export function changeDiff(change: HarnessChange) {
  const split = (text: string | null) => {
    if (text === null || text === "") return [];
    const endsWithNewline = text.endsWith("\n"),
      lines = (endsWithNewline ? text.slice(0, -1) : text).split("\n");
    return lines.map((text, index) => ({
      text,
      noNewline: index === lines.length - 1 && !endsWithNewline,
    }));
  };
  const before = split(change.before),
    after = split(change.after),
    equal = (
      left: { text: string; noNewline: boolean },
      right: { text: string; noNewline: boolean },
    ) => left.text === right.text && left.noNewline === right.noNewline;
  let prefix = 0,
    suffix = 0;
  while (
    prefix < before.length &&
    prefix < after.length &&
    equal(before[prefix], after[prefix])
  )
    prefix++;
  while (
    suffix < before.length - prefix &&
    suffix < after.length - prefix &&
    equal(before[before.length - suffix - 1], after[after.length - suffix - 1])
  )
    suffix++;
  const start = Math.max(0, prefix - 3),
    tail = Math.min(suffix, 3),
    beforeEnd = before.length - suffix,
    afterEnd = after.length - suffix;
  const lines: DiffLine[] = [
    ...before
      .slice(start, prefix)
      .map((line) => ({ kind: "context" as const, ...line })),
    ...before
      .slice(prefix, beforeEnd)
      .map((line) => ({ kind: "remove" as const, ...line })),
    ...after
      .slice(prefix, afterEnd)
      .map((line) => ({ kind: "add" as const, ...line })),
    ...before
      .slice(beforeEnd, beforeEnd + tail)
      .map((line) => ({ kind: "context" as const, ...line })),
  ];
  return {
    header: `@@ -${before.length ? start + 1 : 0},${beforeEnd - start + tail} +${after.length ? start + 1 : 0},${afterEnd - start + tail} @@`,
    lines,
  };
}
