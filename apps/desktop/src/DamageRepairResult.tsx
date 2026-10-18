import { formatNumber, t } from "./i18n";
import type { DamageRepairResult as Result } from "./taskTypes";
import "./integrity-result.css";

function valid(value: unknown): value is Result {
  if (!value || typeof value !== "object") return false;
  const result = value as Result;
  return (
    typeof result.id === "string" &&
    typeof result.backup_directory === "string" &&
    typeof result.assets_enumerated === "boolean" &&
    [
      result.files_repaired,
      result.files_already_valid,
      result.files_missing,
      result.unsupported_total,
    ].every((n) => Number.isSafeInteger(n) && n >= 0) &&
    Array.isArray(result.unsupported) &&
    result.unsupported.length <= 32 &&
    result.unsupported.every(
      (item) =>
        item &&
        typeof item.path === "string" &&
        typeof item.reason === "string",
    )
  );
}

export function DamageRepairResult({ result }: { result: unknown }) {
  if (!valid(result)) return <p role="alert">{t("damage.unavailable")}</p>;
  return (
    <section className="ce-integrity-result" aria-label={t("damage.result")}>
      <p>
        {t("damage.complete", { count: formatNumber(result.files_repaired) })}
      </p>
      <dl className="ce-integrity-summary">
        <div>
          <dt>{t("damage.valid")}</dt>
          <dd>{formatNumber(result.files_already_valid)}</dd>
        </div>
        <div>
          <dt>{t("integrity.missing")}</dt>
          <dd>{formatNumber(result.files_missing)}</dd>
        </div>
        <div>
          <dt>{t("integrity.unverifiable")}</dt>
          <dd>{formatNumber(result.unsupported_total)}</dd>
        </div>
      </dl>
      {result.files_repaired > 0 && (
        <p>{t("damage.backup", { path: result.backup_directory })}</p>
      )}
      {result.files_missing > 0 && <p>{t("damage.missingHelp")}</p>}
      {!result.assets_enumerated && (
        <p className="auth-error">{t("integrity.coverage")}</p>
      )}
      {result.unsupported_total > 0 && (
        <details className="ce-task-repair-details">
          <summary>
            {t("integrity.problems", {
              count: formatNumber(result.unsupported_total),
            })}
          </summary>
          <ul>
            {result.unsupported.map((item, index) => (
              <li key={`${index}:${item.path}`}>
                <span>{item.path}</span>
                <small>{item.reason}</small>
              </li>
            ))}
          </ul>
          {result.unsupported_total > result.unsupported.length && (
            <p>
              {t("integrity.omitted", {
                count: formatNumber(
                  result.unsupported_total - result.unsupported.length,
                ),
              })}
            </p>
          )}
        </details>
      )}
    </section>
  );
}
