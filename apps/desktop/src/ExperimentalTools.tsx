/** Compatibility gateway for launcher navigation and Porter intake. Maker owns
 * its IDE separately; Porter retains its existing draft and browsing contract. */
import { useRef, type RefObject } from "react";
import { ExperimentalExtensions } from "./ExperimentalExtensions";
import { ExperimentalPorterWorkspace } from "./ExperimentalPorterWorkspace";
import { ExperimentalProjects } from "./ExperimentalProjects";
import { MakerSource } from "./MakerSource";
import { createProjectManagerDraft } from "./makerWorkspaceState";
import type { Api } from "./types";
import type { PorterSourceOwner, SourceRelease } from "./porterSourceOwner";
import type {
  ImportedSource,
  PorterOrigin,
  PorterIntake,
} from "./experimentalPorterTypes";
import type {
  ExperimentalNavigate,
  ExperimentalPage,
  MakerSpec,
} from "./experimentalTypes";
import type { ProjectSourceSelection } from "./experimentalProjectTypes";
export type ExperimentalDraft = {
  spec?: MakerSpec;
  source: ImportedSource | null;
  directory: string;
  targetId: string;
  rights: string;
  beta: boolean;
  identifierDeclared?: boolean;
  allowed: string[];
  mode: string;
  prompt: string;
  jobId: string;
  porterProjectId?: string;
  porterName?: string;
  porterGoal?: string;
  porterOriginUrl?: string;
  porterNew?: boolean;
};
type ExperimentalToolsProps = {
  api: Api;
  native: boolean;
  page: ExperimentalPage;
  drafts: RefObject<Partial<Record<ExperimentalPage, ExperimentalDraft>>>;
  sourceOwner: PorterSourceOwner;
  releaseSource: SourceRelease;
  initialSource?: ProjectSourceSelection;
  onConfigureAi?: () => void;
  onNavigate: ExperimentalNavigate;
  onPorterBrowseMods?: (origin?: PorterOrigin | null) => void;
  porterIntake?: PorterIntake | null;
  onPorterIntakeConsumed?: () => void;
};
export function ExperimentalTools(props: ExperimentalToolsProps) {
  const draft = useRef(createProjectManagerDraft());
  if (props.page === "extensions")
    return (
      <ExperimentalExtensions
        api={props.api}
        native={props.native}
        onNavigate={props.onNavigate}
      />
    );
  if (props.initialSource)
    return (
      <MakerSource
        api={props.api}
        native={props.native}
        source={props.initialSource}
        draft={draft.current}
        readOnly={props.initialSource.workflow === "porter"}
      />
    );
  if (props.page === "porter")
    return (
      <ExperimentalPorterWorkspace
        api={props.api}
        native={props.native}
        drafts={props.drafts}
        sourceOwner={props.sourceOwner}
        releaseSource={props.releaseSource}
        onConfigureAi={props.onConfigureAi}
        onBrowseMods={props.onPorterBrowseMods}
        intake={props.porterIntake}
        onIntakeConsumed={props.onPorterIntakeConsumed}
      />
    );
  return (
    <ExperimentalProjects
      api={props.api}
      native={props.native}
      draft={draft}
      onConfigureAi={props.onConfigureAi}
    />
  );
}
