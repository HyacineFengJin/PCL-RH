import { Fragment, useEffect, useRef, useState } from "react";
import { Check, FileCode2, RefreshCw } from "lucide-react";
import { useCode } from "../controller";
import {
  changeDiff,
  jobStatusLabel,
  type HarnessProposalSet,
} from "../harnessView";
import { label } from "../locale";
import type { JobView } from "../../experimentalTypes";

/** A review binds to its captured job. Project switches retire both reads and
 * apply callbacks; only the controller can submit the exact reviewed digest. */
export function HarnessProposalReview({ job }: { job: JobView }) {
  const c = useCode(),
    [data, setData] = useState<{
      scope: string;
      value: HarnessProposalSet;
    } | null>(null),
    [error, setError] = useState(""),
    [applying, setApplying] = useState(""),
    [retry, setRetry] = useState(0),
    scope = c.projectId + ":" + job.summary.jobId,
    current = useRef(scope),
    jobId = job.summary.jobId;
  current.current = scope;
  useEffect(() => {
    setApplying("");
  }, [scope]);
  useEffect(() => {
    let alive = true;
    setError("");
    if (c.native)
      void c
        .readHarnessProposals(jobId)
        .then((value) => {
          if (alive) setData({ scope, value });
        })
        .catch((error) => {
          if (alive) setError(String(error));
        });
    return () => {
      alive = false;
    };
  }, [c.api, c.native, scope, job.summary.revision, retry]);
  const proposals = data?.scope === scope ? data.value.proposals : [];
  async function apply(id: string, digest: string) {
    if (applying) return;
    const ownedScope = scope;
    setApplying(id);
    setError("");
    try {
      await c.applyHarnessProposal(jobId, id, digest);
      if (current.current === ownedScope) setRetry((value) => value + 1);
    } catch (error) {
      if (current.current === ownedScope) setError(String(error));
    } finally {
      if (current.current === ownedScope) setApplying("");
    }
  }
  if (!proposals.length && !error) return null;
  return (
    <section
      className="rc-harness-review"
      aria-label={label("项目改动审阅", "Project change review")}
    >
      <header>
        <FileCode2 size={15} />
        <strong>{label("项目改动建议", "Proposed project changes")}</strong>
      </header>
      {data?.scope === scope && data.value.truncated && (
        <p className="rc-muted">
          {label(
            "部分建议未显示，请分成更小的改动后重新审阅。",
            "Some proposals are not shown. Request smaller changes to review them.",
          )}
        </p>
      )}
      {error && (
        <div className="rc-harness-error" role="alert">
          <p>{error}</p>
          <button
            className="rc-button"
            onClick={() => setRetry((value) => value + 1)}
          >
            <RefreshCw size={13} />
            {label("重新读取", "Reload")}
          </button>
        </div>
      )}
      {proposals.map((proposal) => (
        <article key={proposal.id}>
          <header>
            <strong>
              {proposal.summary || label("源码改动", "Source changes")}
            </strong>
            <span>{jobStatusLabel(proposal.status)}</span>
          </header>
          <div className="rc-harness-file-count">
            {proposal.changeCount ?? proposal.changes.length}{" "}
            {label("个文件", "files")}
          </div>
          {proposal.changesTruncated && (
            <p className="rc-muted">
              {label(
                "文件列表不完整，请缩小建议后重新审阅。",
                "The file list is incomplete. Request a smaller proposal to review.",
              )}
            </p>
          )}
          {proposal.changes.map((change, index) => {
            const diff = changeDiff(change);
            return (
              <details
                className="rc-harness-file-diff"
                key={change.path}
                open={index === 0}
              >
                <summary>
                  <span data-kind={change.kind}>
                    {change.kind === "create"
                      ? label("新增", "Add")
                      : change.kind === "delete"
                        ? label("删除", "Delete")
                        : label("修改", "Modify")}
                  </span>
                  <code>{change.path}</code>
                </summary>
                {(change.beforeTruncated || change.afterTruncated) && (
                  <p className="rc-muted">
                    {label(
                      "这里只显示部分差异；应用会使用完整建议。",
                      "This diff preview is partial. Applying uses the complete proposal.",
                    )}
                  </p>
                )}
                <pre
                  aria-label={
                    label("文件差异", "File diff") + " · " + change.path
                  }
                >
                  <span className="rc-harness-diff-header">{diff.header}</span>
                  {diff.lines.map((line, index) => (
                    <Fragment key={index}>
                      <span className={"rc-harness-diff-line " + line.kind}>
                        {line.kind === "remove"
                          ? "−"
                          : line.kind === "add"
                            ? "+"
                            : " "}
                        {line.text}
                      </span>
                      {line.noNewline && (
                        <span className="rc-harness-diff-newline">
                          {label(
                            "\\ 文件末尾没有换行",
                            "\\ No newline at end of file",
                          )}
                        </span>
                      )}
                    </Fragment>
                  ))}
                </pre>
              </details>
            );
          })}
          {proposal.status === "applied" ? (
            <p className="rc-harness-applied">
              <Check size={14} />
              {label("已应用到项目", "Applied to project")}
            </p>
          ) : (
            <footer>
              <p>
                {label(
                  "应用前会检查原文件；文件已变化时请重新生成建议。",
                  "Original files are checked before applying. Regenerate the proposal if they have changed.",
                )}
              </p>
              <button
                className="rc-button orange"
                disabled={
                  !c.native ||
                  !!c.busy ||
                  !!applying ||
                  job.summary.status !== "completed" ||
                  proposal.changesTruncated ||
                  proposal.status !== "pending"
                }
                onClick={() => void apply(proposal.id, proposal.digest)}
              >
                <Check size={14} />
                {applying === proposal.id
                  ? label("正在应用…", "Applying…")
                  : label("审阅并应用到项目", "Approve and apply to project")}
              </button>
            </footer>
          )}
        </article>
      ))}
    </section>
  );
}
