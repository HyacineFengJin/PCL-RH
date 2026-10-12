import { useEffect, useRef, useState } from "react";
import { experimentalCall, type JobView } from "../../experimentalTypes";
import { useCode } from "../controller";
import { conversationJobIds, newestJob } from "../harnessView";

/** Live jobs stay owned by the controller. Views fetch bounded historical
 * receipts once and ignore replies from a project or conversation they left. */
export function useProjectJobs(conversationId?: string) {
  const c = useCode(),
    [history, setHistory] = useState<{
      scope: string;
      jobs: Record<string, JobView>;
    }>({ scope: "", jobs: {} }),
    [error, setError] = useState(""),
    [retry, setRetry] = useState(0),
    current = useRef(c.jobs);
  current.current = c.jobs;
  const scope = c.projectId + ":" + (conversationId ?? ""),
    conversations =
      c.project?.conversations.filter(
        (value) => !conversationId || value.id === conversationId,
      ) ?? [],
    ids = [
      ...new Set([
        ...conversationJobIds(conversations).slice(-12),
        ...conversations.flatMap((conversation) =>
          conversation.activeJob ? [conversation.activeJob] : [],
        ),
      ]),
    ],
    identity = JSON.stringify(ids);
  useEffect(() => {
    let alive = true;
    setError("");
    if (c.native && ids.length)
      void Promise.allSettled(
        ids.map(async (jobId) => {
          const cached = current.current[jobId];
          const job =
            cached ??
            (await experimentalCall<JobView>(c.api, "job_read", { jobId }));
          if (job.summary.jobId !== jobId)
            throw new Error("Task identity mismatch");
          return job;
        }),
      ).then((results) => {
        if (!alive) return;
        const jobs: Record<string, JobView> = {};
        for (const result of results)
          if (result.status === "fulfilled")
            jobs[result.value.summary.jobId] = result.value;
          else setError(String(result.reason));
        setHistory({ scope, jobs });
      });
    return () => {
      alive = false;
    };
  }, [c.api, c.native, scope, identity, retry]);
  return {
    jobs: ids
      .flatMap((id) => {
        const job = newestJob(
          history.scope === scope ? history.jobs[id] : undefined,
          c.jobs[id],
        );
        return job ? [job] : [];
      })
      .reverse(),
    error,
    refresh: () => setRetry((value) => value + 1),
  };
}
