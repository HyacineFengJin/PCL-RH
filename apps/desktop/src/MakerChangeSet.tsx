/** Unit confirmations stage consent to one immutable review digest. Closing a
 * diff is read-only. Only explicit return revokes a proposal; archive recovery
 * goes through the host's idempotent receipt reconciliation. */
import { useEffect, useRef, useState } from "react";
import {
  Check,
  ChevronRight,
  FileDiff,
  LockKeyhole,
  RotateCcw,
} from "lucide-react";
import { experimentalCall as call, type ReviewView } from "./experimentalTypes";
import { groupReview, type MakerScopeHint } from "./MakerAgentState";
import type { Api } from "./types";
import { useMakerScope } from "./useMakerScope";
import { t, serviceError } from "./i18n";
export function MakerChangeSet({
  api,
  native,
  threadId,
  jobId,
  reviewId,
  units,
  finished,
  receiptExists = false,
  recorded = false,
  onDiff,
  onFollowup,
  onCommitted,
  onPending,
  onFiles,
}: {
  api: Api;
  native: boolean;
  threadId: string;
  jobId: string;
  reviewId: string;
  units: MakerScopeHint[];
  finished: boolean;
  receiptExists?: boolean;
  recorded?: boolean;
  onDiff: (title: string, text: string) => void;
  onFollowup: (text: string) => void;
  onCommitted: () => Promise<void>;
  onPending: (jobId: string, pending: boolean) => void;
  onFiles: (paths: string[], locks: string[]) => void;
}) {
  const scope = useMakerScope(api, native, `${threadId}:${jobId}:${reviewId}`);
  const [review, setReview] = useState<ReviewView | null>(null),
    [approved, setApproved] = useState<string[]>([]),
    [uncertain, setUncertain] = useState(false);
  const submitted = useRef(false);
  async function read(current = scope.readTicket()) {
    const value = await call<ReviewView & { jobId: string }>(
      api,
      "review_read",
      { jobId, reviewId },
    );
    if (!current()) return;
    if (value.jobId !== jobId || value.reviewId !== reviewId)
      throw new Error(t("maker.responseMismatch"));
    if (review?.reviewDigest !== value.reviewDigest) setApproved([]);
    setReview(value);
    setUncertain(false);
    submitted.current = false;
    onPending(jobId, ["pending", "approved"].includes(value.status));
    onFiles(
      (value.preview.diffs || value.preview.changes || []).map(
        (change) => change.path,
      ),
      value.preview.locked_files ?? [],
    );
  }
  useEffect(() => {
    const current = scope.readTicket();
    if (native)
      void read(current).catch((e) => {
        if (current()) scope.setError(serviceError(e));
      });
  }, [scope.scope]);
  const groups = review ? groupReview(review, units) : [];
  const pending = review?.status === "pending",
    applied = review?.status === "applied" || receiptExists;
  return (
    <section className="ma-change-card ma-flow-card">
      <header>
        <FileDiff size={16} />
        <strong>{t("agent.changeSet")}</strong>
        <span className="ma-badge pending">
          {review
            ? t(`agent.reviewState.${review.status as "pending"}`)
            : t("agent.loading")}
        </span>
      </header>
      {review && (
        <>
          <p className="ma-muted">{t("agent.atomicReview")}</p>
          {groups.map((group) => (
            <div className="ma-review-unit" key={group.id}>
              <div>
                <strong>{group.name || t("agent.unassigned")}</strong>
                <small>
                  {t("agent.fileCount", { count: group.changes.length })}
                </small>
                {approved.includes(group.id) && <Check size={15} />}
              </div>
              <details>
                <summary>
                  <ChevronRight size={13} />
                  {t("agent.viewDiff")}
                </summary>
                {group.changes.map((change) => (
                  <button
                    className="ma-diff-row"
                    key={change.path}
                    onClick={() =>
                      onDiff(
                        change.path,
                        ("diff" in change
                          ? change.diff
                          : "unified_diff" in change
                            ? change.unified_diff
                            : "") ?? "",
                      )
                    }
                  >
                    <FileDiff size={13} />
                    <span>{change.path}</span>
                  </button>
                ))}
              </details>
              {pending && (
                <div className="ma-review-actions">
                  <button
                    className="ce-button ma-approve"
                    disabled={!finished || scope.busy || uncertain}
                    onClick={() => {
                      if (scope.callbackCurrent() && !scope.busy && !uncertain)
                        setApproved((old) =>
                          old.includes(group.id)
                            ? old.filter((id) => id !== group.id)
                            : [...old, group.id],
                        );
                    }}
                  >
                    {approved.includes(group.id)
                      ? t("agent.unapprove")
                      : t("agent.approveUnit")}
                  </button>
                  <button
                    className="ma-text-button"
                    onClick={() =>
                      onFollowup(
                        t("agent.followupUnit", {
                          name: group.name || t("agent.unassigned"),
                        }),
                      )
                    }
                  >
                    {t("agent.followup")}
                  </button>
                  <button
                    className="ma-text-button ma-risk"
                    disabled={!finished || scope.busy || uncertain}
                    onClick={() =>
                      void scope.perform(async () => {
                        await call(api, "review_cancel", { jobId, reviewId });
                        await read();
                      })
                    }
                  >
                    {t("agent.returnSet")}
                  </button>
                </div>
              )}
            </div>
          ))}
          {review.status === "approved" && (
            <button
              className="ma-text-button ma-risk"
              disabled={scope.busy}
              onClick={() =>
                void scope.perform(async () => {
                  await call(api, "review_cancel", { jobId, reviewId });
                  await read();
                })
              }
            >
              {t("agent.returnSet")}
            </button>
          )}
          {review.preview.locked_files?.length ? (
            <p className="ma-lock-note">
              <LockKeyhole size={13} />
              {t("agent.protectedCount", {
                count: review.preview.locked_files.length,
              })}
            </p>
          ) : null}
          {review.preview.warnings?.map((value, i) => (
            <p className="ma-risk" key={i}>
              {value}
            </p>
          ))}
          {recorded ? (
            <p className="ma-muted">{t("agent.archived")}</p>
          ) : pending || applied ? (
            <button
              className="ce-button ma-pill ma-approve"
              disabled={
                !native ||
                !finished ||
                scope.busy ||
                uncertain ||
                submitted.current ||
                (pending &&
                  !groups.every((group) => approved.includes(group.id)))
              }
              onClick={() => {
                if (!scope.callbackCurrent() || submitted.current || !review)
                  return;
                submitted.current = true;
                setUncertain(true);
                void scope.perform(async () => {
                  await call(api, "maker_thread_apply", {
                    threadId,
                    jobId,
                    reviewId,
                    digest: review.reviewDigest,
                  });
                  if (!scope.responseCurrent()) return;
                  setReview({ ...review, status: "applied" });
                  onPending(jobId, false);
                  await onCommitted();
                });
              }}
            >
              {applied ? t("agent.reconcileArchive") : t("agent.applyArchive")}
            </button>
          ) : null}
          {uncertain && (
            <p className="ma-muted">{t("agent.uncertainReview")}</p>
          )}
        </>
      )}
      {scope.error && (
        <p className="experimental-error" role="alert">
          {scope.error}
        </p>
      )}
      <button
        className="ma-text-button"
        disabled={!native || scope.busy}
        onClick={() => void scope.perform(() => read())}
      >
        <RotateCcw size={12} />
        {t("maker.refreshReview")}
      </button>
    </section>
  );
}
