import { useEffect, useState } from "react";
import { Layers, ExternalLink, FolderInput } from "lucide-react";
import { useCode } from "../controller";
import { label } from "../locale";
import {
  experimentalCall,
  type EngineStatus,
  type JobView,
  type ReviewView,
} from "../../experimentalTypes";
import { MakerChangeSet } from "../../MakerChangeSet";
export function Output() {
  const c = useCode(),
    [engine, setEngine] = useState<EngineStatus | null>(null);
  useEffect(() => {
    let alive = true;
    if (c.native)
      void experimentalCall<EngineStatus>(c.api, "status")
        .then((e) => {
          if (alive) setEngine(e);
        })
        .catch((e) => c.setError(String(e)));
    return () => {
      alive = false;
    };
  }, [c.native, c.projectId, c.snapshot?.revision]);
  const ids = new Set(
    c.project?.conversations.flatMap((t) =>
      t.messages.map((m) => m.jobId).filter(Boolean),
    ),
  );
  return (
    <div className="rc-panel-pad">
      <header className="rc-section-title">
        {label("项目任务", "Project tasks")}
      </header>
      {engine?.jobs
        .filter((j) => ids.has(j.jobId))
        .map((j) => (
          <div className="rc-task" key={j.jobId}>
            <span className={j.status === "running" ? "rc-live-dot" : ""} />
            <span>
              {j.workflow}
              <small>{j.status}</small>
            </span>
            <code>{j.jobId.slice(0, 8)}</code>
            {["queued", "running"].includes(j.status) && (
              <button
                className="rc-button"
                onClick={() =>
                  void c.stop(j.jobId).catch((e) => c.setError(String(e)))
                }
              >
                {label("停止", "Stop")}
              </button>
            )}
          </div>
        ))}
      {!ids.size && (
        <p className="rc-muted">
          {label("尚无项目任务", "No project tasks yet")}
        </p>
      )}
    </div>
  );
}
export function Changes() {
  const c = useCode(),
    [jobs, setJobs] = useState<JobView[]>([]),
    [review, setReview] = useState<{ job: JobView; review: ReviewView } | null>(
      null,
    ),
    [diff, setDiff] = useState("");
  const last =
    c.project?.conversations.flatMap((t) =>
      t.messages.map((m) => m.jobId).filter((id): id is string => !!id),
    ) ?? [];
  useEffect(() => {
    let alive = true;
    if (c.native)
      void Promise.all(
        [...new Set(last)]
          .slice(-12)
          .map((jobId) =>
            experimentalCall<JobView>(c.api, "job_read", { jobId }),
          ),
      )
        .then((v) => {
          if (alive) setJobs(v);
        })
        .catch((e) => c.setError(String(e)));
    return () => {
      alive = false;
    };
  }, [c.native, c.projectId, c.project?.conversations]);
  async function show(job: JobView, id: string) {
    setReview({
      job,
      review: await experimentalCall<ReviewView>(c.api, "review_read", {
        jobId: job.summary.jobId,
        reviewId: id,
      }),
    });
  }
  return (
    <div className="rc-panel-pad">
      <header className="rc-section-title">
        {label("任务产物与改动", "Artifacts and changes")}
      </header>
      {jobs.map((job) => (
        <section className="rc-artifacts" key={job.summary.jobId}>
          <header>
            {job.summary.status} · {job.summary.jobId.slice(0, 8)}
          </header>
          {job.artifacts.map((a) => (
            <button
              className="rc-button"
              key={a.operationId}
              onClick={() =>
                void c
                  .api("experimental_open", {
                    jobId: job.summary.jobId,
                    operationId: a.operationId,
                  })
                  .catch((e) => c.setError(String(e)))
              }
            >
              <ExternalLink size={14} />
              {a.operationId}
            </button>
          ))}
          {job.artifacts.map((a) => (
            <button
              className="rc-button purple"
              key={"adopt-" + a.operationId}
              disabled={
                !c.native ||
                !!c.busy ||
                ["queued", "running"].includes(job.summary.status)
              }
              onClick={() =>
                void c
                  .adoptSource(job.summary.jobId, a.operationId)
                  .catch(() => {})
              }
            >
              <FolderInput size={14} />
              {label("采用源码到空文件夹", "Adopt into empty folder")}
            </button>
          ))}
          {job.reviews.map((id) => (
            <button
              className="rc-button orange"
              key={id}
              onClick={() =>
                void show(job, id).catch((e) => c.setError(String(e)))
              }
            >
              <Layers size={14} />
              {label("审阅补丁", "Review patch")}
            </button>
          ))}
        </section>
      ))}
      {!jobs.length && (
        <p className="rc-muted">
          {label("还没有任务产物", "No task artifacts yet")}
        </p>
      )}
      {review && (
        <MakerChangeSet
          api={c.api}
          native={c.native}
          threadId={c.projectId}
          jobId={review.job.summary.jobId}
          reviewId={review.review.reviewId}
          units={[]}
          finished={!["queued", "running"].includes(review.job.summary.status)}
          onPending={() => {}}
          onFiles={() => {}}
          onCommitted={async () => {
            setReview(null);
            await c.read();
          }}
          onDiff={(_title, text) => setDiff(text)}
          onFollowup={(text) => {
            const id = c.project?.conversations[0]?.id;
            if (id) {
              c.setDraft(id, text);
              c.openConversation(id);
            }
          }}
        />
      )}{" "}
      {diff && <pre className="rc-diff">{diff}</pre>}
    </div>
  );
}
