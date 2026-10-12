import { useRef, useState } from "react";
import { Layers, ExternalLink, FolderInput, RefreshCw } from "lucide-react";
import { useCode } from "../controller";
import { label } from "../locale";
import {
  experimentalCall,
  type JobView,
  type ReviewView,
} from "../../experimentalTypes";
import { MakerChangeSet } from "../../MakerChangeSet";
import { isActiveJob, jobStatusLabel, projectJobOwner } from "../harnessView";
import { HarnessProposalReview } from "./HarnessProposalReview";
import { JobActivity } from "./JobActivity";
import { useProjectJobs } from "./useProjectJobs";

function ReadError({ error, retry }: { error: string; retry: () => void }) {
  return error ? (
    <div className="rc-harness-error" role="alert">
      <p>{error}</p>
      <button className="rc-button" onClick={retry}>
        <RefreshCw size={13} />
        {label("重新读取", "Reload")}
      </button>
    </div>
  ) : null;
}

export function Output() {
  const c = useCode(),
    { jobs, error, refresh } = useProjectJobs();
  return (
    <div className="rc-panel-pad rc-harness-tasks">
      <header className="rc-section-title">
        {label("项目任务", "Project tasks")}
      </header>
      <ReadError error={error} retry={refresh} />
      {jobs.map((job) => (
        <article key={job.summary.jobId}>
          <header className="rc-harness-task-title">
            <strong>
              {c.project &&
                projectJobOwner(c.project, job.summary.jobId)?.title}
            </strong>
            <span>
              {job.summary.workflow === "harness"
                ? "RH Code"
                : job.summary.workflow}
            </span>
          </header>
          <JobActivity job={job} jobId={job.summary.jobId} onStop={c.stop} />
        </article>
      ))}
      {!jobs.length && !error && (
        <p className="rc-muted">
          {label("尚无项目任务", "No project tasks yet")}
        </p>
      )}
    </div>
  );
}

export function Changes() {
  const c = useCode(),
    { jobs, error, refresh } = useProjectJobs(),
    [review, setReview] = useState<{
      scope: string;
      job: JobView;
      review: ReviewView;
    } | null>(null),
    [diff, setDiff] = useState<{ scope: string; text: string } | null>(null),
    current = useRef(c.projectId);
  current.current = c.projectId;
  async function show(job: JobView, id: string) {
    const scope = c.projectId,
      value = await experimentalCall<ReviewView>(c.api, "review_read", {
        jobId: job.summary.jobId,
        reviewId: id,
      });
    if (current.current === scope) {
      setReview({ scope, job, review: value });
      setDiff(null);
    }
  }
  const visibleReview = review?.scope === c.projectId ? review : null;
  return (
    <div className="rc-panel-pad">
      <header className="rc-section-title">
        {label("任务产物与改动", "Artifacts and changes")}
      </header>
      <ReadError error={error} retry={refresh} />
      {jobs.map((job) => (
        <section className="rc-artifacts" key={job.summary.jobId}>
          <header>
            {c.project && projectJobOwner(c.project, job.summary.jobId)?.title}{" "}
            · {jobStatusLabel(job.summary.status)} ·{" "}
            {job.summary.jobId.slice(0, 8)}
          </header>
          {job.summary.workflow === "harness" ? (
            <HarnessProposalReview
              key={c.projectId + ":" + job.summary.jobId}
              job={job}
            />
          ) : (
            <>
              {job.artifacts.map((artifact) => (
                <button
                  className="rc-button"
                  key={artifact.operationId}
                  onClick={() =>
                    void c
                      .api("experimental_open", {
                        jobId: job.summary.jobId,
                        operationId: artifact.operationId,
                      })
                      .catch((error) => c.setError(String(error)))
                  }
                >
                  <ExternalLink size={14} />
                  {artifact.operationId}
                </button>
              ))}
              {job.summary.workflow === "maker" &&
                job.artifacts.map((artifact) => (
                  <button
                    className="rc-button purple"
                    key={"adopt-" + artifact.operationId}
                    disabled={
                      !c.native || !!c.busy || isActiveJob(job.summary.status)
                    }
                    onClick={() =>
                      void c
                        .adoptSource(job.summary.jobId, artifact.operationId)
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
                    void show(job, id).catch((error) =>
                      c.setError(String(error)),
                    )
                  }
                >
                  <Layers size={14} />
                  {label("审阅补丁", "Review patch")}
                </button>
              ))}
            </>
          )}
        </section>
      ))}
      {!jobs.length && !error && (
        <p className="rc-muted">
          {label("还没有任务产物", "No task artifacts yet")}
        </p>
      )}
      {visibleReview && (
        <MakerChangeSet
          key={visibleReview.scope + ":" + visibleReview.review.reviewId}
          api={c.api}
          native={c.native}
          threadId={c.projectId}
          jobId={visibleReview.job.summary.jobId}
          reviewId={visibleReview.review.reviewId}
          units={[]}
          finished={!isActiveJob(visibleReview.job.summary.status)}
          onPending={() => {}}
          onFiles={() => {}}
          onCommitted={async () => {
            setReview(null);
            await c.read();
            refresh();
          }}
          onDiff={(_title, text) =>
            setDiff({ scope: visibleReview.scope, text })
          }
          onFollowup={(text) => {
            if (current.current !== visibleReview.scope) return;
            const id = c.project?.conversations[0]?.id;
            if (id) {
              c.setDraft(id, text);
              c.openConversation(id);
            }
          }}
        />
      )}
      {diff?.scope === c.projectId && (
        <pre className="rc-diff">{diff.text}</pre>
      )}
    </div>
  );
}
