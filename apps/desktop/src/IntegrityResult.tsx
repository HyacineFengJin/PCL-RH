import { formatNumber, t, type MessageKey } from "./i18n";
import type { FileIntegrityResult } from "./taskTypes";
import "./integrity-result.css";

const labels: Record<
  FileIntegrityResult["problems"][number]["kind"],
  MessageKey
> = {
  missing: "integrity.missing",
  size_mismatch: "integrity.sizeMismatch",
  sha1_mismatch: "integrity.sha1Mismatch",
  unverifiable: "integrity.unverifiable",
};

export function isIntegrityResult(
  value: unknown,
): value is FileIntegrityResult {
  if (!value || typeof value !== "object") return false;
  const report = value as FileIntegrityResult;
  return (
    typeof report.id === "string" &&
    typeof report.assets_enumerated === "boolean" &&
    [
      report.files_verified,
      report.files_missing,
      report.files_corrupt,
      report.files_unverifiable,
      report.problems_total,
    ].every((n) => Number.isSafeInteger(n) && n >= 0) &&
    Array.isArray(report.problems) &&
    report.problems.length <= 32 &&
    report.problems.every(
      (problem) =>
        problem &&
        typeof problem.path === "string" &&
        typeof problem.reason === "string" &&
        Object.hasOwn(labels, problem.kind),
    )
  );
}

/** Completed checks retain a readable result under their native task ID. */
export function IntegrityResult({ result }: { result: unknown }) {
  if (!isIntegrityResult(result))
    return <p role="alert">{t("integrity.resultUnavailable")}</p>;
  const omitted = Math.max(0, result.problems_total - result.problems.length);
  return (
    <section className="ce-integrity-result" aria-label={t("integrity.result")}>
      <p>{t("integrity.complete")}</p>
      <dl className="ce-integrity-summary">
        {(
          [
            ["integrity.verified", result.files_verified],
            ["integrity.missing", result.files_missing],
            ["integrity.corrupt", result.files_corrupt],
            ["integrity.unverifiable", result.files_unverifiable],
          ] as const
        ).map(([key, count]) => (
          <div key={key}>
            <dt>{t(key)}</dt>
            <dd>{formatNumber(count)}</dd>
          </div>
        ))}
      </dl>
      {!result.assets_enumerated && (
        <p className="auth-error">{t("integrity.coverage")}</p>
      )}
      {result.problems_total > 0 && (
        <details className="ce-task-repair-details" open>
          <summary>
            {t("integrity.problems", {
              count: formatNumber(result.problems_total),
            })}
          </summary>
          <ul>
            {result.problems.map((problem, index) => (
              <li key={`${index}:${problem.path}`}>
                <span>{problem.path}</span>
                <small>
                  {t(labels[problem.kind])} · {problem.reason}
                </small>
              </li>
            ))}
          </ul>
          {omitted > 0 && (
            <p>{t("integrity.omitted", { count: formatNumber(omitted) })}</p>
          )}
        </details>
      )}
    </section>
  );
}
