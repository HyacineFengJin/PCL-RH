import { useEffect, useState } from "react";
import {
  Check,
  FileCode2,
  ListChecks,
  Sparkles,
  Square,
  ShieldCheck,
  ArrowUpRight,
} from "lucide-react";
import type { Api } from "./types";
import { experimentalCall as call, type JobView } from "./experimentalTypes";
import { useMakerScope } from "./useMakerScope";
import {
  type MakerThread,
  type MakerRound,
  type MakerScopeHint,
  type MakerActivity,
} from "./MakerAgentState";
import { MakerChangeSet } from "./MakerChangeSet";
import { t, serviceError } from "./i18n";
export function MakerThreadView({
  thread,
  ...props
}: { thread: MakerThread } & Omit<
  Parameters<typeof MakerRoundView>[0],
  "threadId" | "round"
>) {
  const [visible, setVisible] = useState(6);
  useEffect(() => setVisible(6), [thread.id]);
  return (
    <div
      className="ma-conversation"
      key={thread.id}
      aria-label={t("agent.conversation")}
    >
      {thread.rounds.length > visible && (
        <button
          className="ma-text-button"
          onClick={() => setVisible((value) => value + 6)}
        >
          {t("agent.olderRounds", { count: thread.rounds.length - visible })}
        </button>
      )}
      {thread.rounds.slice(-visible).map((round, index) => (
        <MakerRoundView
          key={round.jobId}
          {...props}
          threadId={thread.id}
          round={round}
          latest={index === Math.min(visible, thread.rounds.length) - 1}
        />
      ))}
    </div>
  );
}
export function MakerRoundView({
  api,
  native,
  threadId,
  round,
  units,
  recordedSources,
  latest,
  onDiff,
  onFollowup,
  onCommitted,
  onPending,
  onActivity,
  onSource,
}: {
  api: Api;
  native: boolean;
  threadId: string;
  round: MakerRound;
  units: MakerScopeHint[];
  recordedSources: string[];
  latest?: boolean;
  onDiff: (title: string, text: string) => void;
  onFollowup: (text: string) => void;
  onCommitted: () => Promise<void>;
  onPending: (jobId: string, pending: boolean) => void;
  onActivity: (activity: MakerActivity[], running: boolean) => void;
  onSource: (jobId: string, operationId: string) => void;
}) {
  const scope = useMakerScope(api, native, round.jobId),
    [job, setJob] = useState<JobView | null>(null);
  const [recorded, setRecorded] = useState(false);
  async function read(current = scope.readTicket()) {
    const value = await call<JobView>(api, "job_read", { jobId: round.jobId });
    if (!current()) return;
    if (
      value.summary.jobId !== round.jobId ||
      value.summary.workflow !== "maker"
    )
      throw new Error(t("maker.responseMismatch"));
    setJob(value);
    if (latest)
      onActivity(
        value.activity ?? [],
        ["queued", "running"].includes(value.summary.status),
      );
  }
  useEffect(() => {
    const current = scope.readTicket();
    if (native)
      void read(current).catch((e) => {
        if (current()) scope.setError(serviceError(e));
      });
  }, [scope.scope]);
  const running = !!job && ["queued", "running"].includes(job.summary.status);
  useEffect(() => {
    if (!native || !running) return;
    const timer = setTimeout(() => {
      const current = scope.readTicket();
      void read(current).catch((e) => {
        if (current()) scope.setError(serviceError(e));
      });
    }, 1700);
    return () => clearTimeout(timer);
  }, [job, native]);
  const plans = (job?.activity ?? []).filter((value) => value.steps?.length),
    reads = (job?.activity ?? []).filter(
      (value) => value.name === "workspace.read",
    );
  const outputs = (job?.artifacts ?? []).filter(
    (artifact) =>
      !artifact.operationId.startsWith("host-") ||
      job!.reviews.includes(artifact.operationId.slice(5)),
  );
  const text =
    typeof job?.result === "object" && job.result && "text" in job.result
      ? String(job.result.text)
      : typeof job?.result === "string"
        ? job.result
        : "";
  return (
    <section className="ma-round">
      <div className="ma-user-message">
        <span className="ma-eyebrow">{t(`agent.mode.${round.mode}`)}</span>
        <p>{round.request}</p>
      </div>
      <div className="ma-agent-heading">
        <span className={running ? "ma-morph" : "ma-agent-avatar"}>
          {running ? null : job?.summary.status === "completed" ? (
            <Check size={14} />
          ) : (
            <Sparkles size={14} />
          )}
        </span>
        <strong>Agent</strong>
        <small>
          {job
            ? t(`experimental.job.${job.summary.status as "running"}`)
            : t("agent.loading")}
        </small>
        {running && (
          <button
            className="ma-icon"
            aria-label={t("experimental.cancelJob")}
            disabled={scope.busy}
            onClick={() =>
              void scope.perform(async () => {
                await call(api, "job_cancel", { jobId: round.jobId });
                await read();
              })
            }
          >
            <Square size={13} />
          </button>
        )}
      </div>
      {plans.map((plan) => (
        <section className="ma-flow-card ma-plan-card" key={plan.id}>
          <header>
            <ListChecks size={16} />
            <strong>{t("agent.plan")}</strong>
            <small>{t("agent.intentions")}</small>
          </header>
          <ol>
            {plan.steps!.map((step, index) => (
              <li key={index}>
                <span>{index + 1}</span>
                {step}
              </li>
            ))}
          </ol>
        </section>
      ))}
      {reads.length > 0 && (
        <details className="ma-flow-card ma-read-card">
          <summary>
            <FileCode2 size={15} />
            {t("agent.readRecords", { count: reads.length })}
          </summary>
          <div>
            {reads.map((read) => (
              <p
                className={
                  running && read.status === "running" ? "is-reading" : ""
                }
                key={read.id}
              >
                {read.status === "completed" ? (
                  <Check size={12} />
                ) : (
                  <span
                    className={
                      running ? "ma-morph small" : "ma-read-incomplete"
                    }
                  >
                    {running ? null : "○"}
                  </span>
                )}
                <code>{read.path ?? read.id}</code>
              </p>
            ))}
          </div>
        </details>
      )}
      {text && <div className="ma-agent-text">{text}</div>}
      {job?.reviews.map((reviewId) => (
        <MakerChangeSet
          key={reviewId}
          api={api}
          native={native}
          threadId={threadId}
          jobId={round.jobId}
          reviewId={reviewId}
          units={round.scopes ?? units}
          finished={!running}
          receiptExists={job.artifacts.some(
            (value) => value.operationId === `host-${reviewId}`,
          )}
          recorded={recordedSources.includes(`${round.jobId}:host-${reviewId}`)}
          onDiff={onDiff}
          onFollowup={onFollowup}
          onCommitted={onCommitted}
          onPending={onPending}
          onFiles={(paths, locks) => {
            if (latest)
              onActivity(
                [
                  ...(job.activity ?? []),
                  {
                    id: "changed",
                    name: "review.changed",
                    status: "completed",
                    steps: paths,
                  },
                  {
                    id: "locked",
                    name: "review.locked",
                    status: "completed",
                    steps: locks,
                  },
                ],
                running,
              );
          }}
        />
      ))}
      {job && !running && (
        <section className="ma-flow-card ma-verification">
          <header>
            <ShieldCheck size={16} />
            <strong>{t("agent.verification")}</strong>
          </header>
          <div>
            <span>{t("agent.sourceOutput")}</span>
            <b>
              {outputs.length ? t("agent.sourceProduced") : t("agent.noSource")}
            </b>
          </div>
          <div>
            <span>{t("agent.buildLabel")}</span>
            <b className="ma-muted">{t("agent.notRun")}</b>
          </div>
          <div>
            <span>{t("agent.gameLabel")}</span>
            <b className="ma-muted">{t("agent.notRun")}</b>
          </div>
        </section>
      )}
      {job &&
        !running &&
        outputs.map((artifact) => (
          <div className="ma-artifact-row" key={artifact.operationId}>
            <FileCode2 size={15} />
            <span>
              {t("agent.sourceCopy")} · {artifact.files.length}
            </span>
            <button
              className="ma-text-button"
              onClick={() => onSource(round.jobId, artifact.operationId)}
            >
              {t("agent.openSource")}
              <ArrowUpRight size={12} />
            </button>
            {!job.reviews.length && (
              <button
                className="ce-button ma-approve"
                disabled={
                  scope.busy ||
                  recorded ||
                  recordedSources.includes(
                    `${round.jobId}:${artifact.operationId}`,
                  )
                }
                onClick={() =>
                  void scope.perform(async () => {
                    await call(api, "maker_thread_archive", {
                      threadId,
                      jobId: round.jobId,
                      operationId: artifact.operationId,
                    });
                    setRecorded(true);
                    await onCommitted();
                  })
                }
              >
                {recorded ||
                recordedSources.includes(
                  `${round.jobId}:${artifact.operationId}`,
                )
                  ? t("agent.archived")
                  : t("agent.approveArchive")}
              </button>
            )}
          </div>
        ))}
      {job?.error && <p className="experimental-error">{job.error.message}</p>}
      {scope.error && (
        <p className="experimental-error" role="alert">
          {scope.error}
        </p>
      )}
    </section>
  );
}
