import { useEffect, useState } from "react";
import type { Api } from "./types";
import type { MakerProject } from "./experimentalProjectTypes";
import { experimentalCall as call } from "./experimentalTypes";
import type { MakerUnitPage, MakerUnitResult } from "./makerWorkspaceState";
import type { MakerScopeHint } from "./MakerAgentState";
import { useMakerScope } from "./useMakerScope";
import { t, serviceError } from "./i18n";
/** @ completion reads bounded unit summaries, then the chosen dossier. */
export function MakerMentions({
  api,
  native,
  project,
  query,
  onChoose,
}: {
  api: Api;
  native: boolean;
  project: MakerProject;
  query: string;
  onChoose: (scope: MakerScopeHint) => void;
}) {
  const owner = useMakerScope(
      api,
      native,
      `${project.id}:${project.revision}:${query}`,
    ),
    [page, setPage] = useState<MakerUnitPage | null>(null);
  const ref = { id: project.id, expectedRevision: project.revision };
  useEffect(() => {
    setPage(null);
    const current = owner.readTicket();
    if (native)
      void call<MakerUnitPage>(api, "project_units", {
        ...ref,
        query: query.slice(0, 120),
      })
        .then((value) => {
          if (current()) {
            if (value.id !== project.id || value.revision !== project.revision)
              throw new Error(t("maker.responseMismatch"));
            setPage(value);
          }
        })
        .catch((e) => {
          if (current()) owner.setError(serviceError(e));
        });
  }, [owner.scope]);
  return (
    <div className="ma-mentions" role="group" aria-label={t("agent.reference")}>
      {page?.units.slice(0, 8).map((unit) => (
        <button
          type="button"
          key={unit.id}
          disabled={owner.busy}
          onClick={() =>
            void owner.perform(async () => {
              const value = await call<MakerUnitResult>(
                api,
                "project_unit_read",
                { ...ref, unitId: unit.id },
              );
              if (!owner.responseCurrent()) return;
              if (
                value.id !== project.id ||
                value.revision !== project.revision ||
                value.unit.id !== unit.id
              )
                throw new Error(t("maker.responseMismatch"));
              onChoose(value.unit);
            })
          }
        >
          @{t(`maker.kind.${unit.kind}`)}/{unit.name}
        </button>
      ))}
      {page && !page.units.length && <small>{t("agent.noUnits")}</small>}
      {owner.error && <p role="alert">{owner.error}</p>}
    </div>
  );
}
