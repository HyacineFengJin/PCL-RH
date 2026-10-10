/** Old native records stay in their original stores. Opening the compatibility
 * reader is explicit; registering a new project never migrates or erases them. */
import { useRef, useState } from "react";
import { ExperimentalProjects } from "../ExperimentalProjects";
import { ExperimentalPorterWorkspace } from "../ExperimentalPorterWorkspace";
import { createProjectManagerDraft } from "../makerWorkspaceState";
import type { ExperimentalDraft } from "../ExperimentalTools";
import type { PorterIntake } from "../experimentalPorterTypes";
import { useCode } from "./controller";
import type { Area } from "./types";
import { label } from "./locale";
export function LegacyArchive({
  intake,
  onConsumed,
}: {
  area?: Area;
  intake?: PorterIntake | null;
  onConsumed?: () => void;
}) {
  const c = useCode(),
    [page, setPage] = useState(intake ? "porter" : "maker"),
    maker = useRef(createProjectManagerDraft()),
    porter = useRef<
      Partial<Record<"maker" | "porter" | "extensions", ExperimentalDraft>>
    >({});
  return (
    <div className="rc-legacy">
      <div className="rc-row">
        <button className="rc-button" onClick={() => setPage("maker")}>
          {label("旧版制作项目", "Previous authoring projects")}
        </button>
        <button className="rc-button orange" onClick={() => setPage("porter")}>
          {label("旧版迁移项目", "Previous migration projects")}
        </button>
      </div>
      {page === "maker" ? (
        <ExperimentalProjects api={c.api} native={c.native} draft={maker} />
      ) : (
        <ExperimentalPorterWorkspace
          api={c.api}
          native={c.native}
          drafts={porter}
          intake={intake}
          onIntakeConsumed={onConsumed}
        />
      )}
    </div>
  );
}
