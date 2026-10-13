import { useEffect, useState } from "react";
import { Sparkles } from "lucide-react";
import { CeSelect } from "../../CeSelect";
import { useProjectJobs } from "../plugins/useProjectJobs";
import { projectJobOwner } from "../harnessView";
import { label } from "../locale";
import {
  validatePixel,
  validateModel,
} from "../../../../../code/visual-assets.mjs";
import { assetPath, type AssetKind } from "./documents";
import type { useAssetDocument } from "./useAssetDocument";

type Preview = {
  path: string;
  kind: AssetKind;
  revision: number;
  document?: unknown;
  image: { mimeType: "image/png"; data: string; width: number; height: number };
};
export function AiAssetPreview({
  kind,
  session,
}: {
  kind: AssetKind;
  session: ReturnType<typeof useAssetDocument>;
}) {
  const { jobs, error: readError } = useProjectJobs(),
    [selection, setSelection] = useState({ scope: "", key: "" }),
    [destinationDraft, setDestinationDraft] = useState({ key: "", value: "" }),
    [imported, setImported] = useState(""),
    scope = JSON.stringify([session.c.projectId, kind]);
  const [readScope, setReadScope] = useState(scope);
  // The task hook clears its read error in an effect on navigation. Hide the
  // preceding scope synchronously until that effect has completed as well.
  useEffect(() => setReadScope(scope), [scope]);
  function jobLabel(jobId: string) {
    const owner =
      session.c.project && projectJobOwner(session.c.project, jobId);
    return (
      (owner?.title?.trim() || label("项目任务", "Project task")) +
      " · " +
      jobId.slice(-8)
    );
  }
  const candidates = jobs.flatMap((job) => {
      // Job receipts are bound and validated by the native host. Read only this
      // project's known jobs, as the existing task hook does for review views.
      const artifacts =
        (job as typeof job & { visualArtifacts?: Preview[] }).visualArtifacts ??
        [];
      return artifacts
        .filter((artifact) => artifact.kind === kind)
        .map((artifact) => ({
          artifact,
          jobId: job.summary.jobId,
          key: JSON.stringify([job.summary.jobId, artifact.path]),
          owner: jobLabel(job.summary.jobId),
        }));
    }),
    candidate =
      (selection.scope === scope &&
        candidates.find((value) => value.key === selection.key)) ||
      candidates[0],
    errors = jobs.flatMap((job) => {
      const value = (job as typeof job & { visualError?: string }).visualError;
      return value
        ? [
            {
              key: job.summary.jobId,
              owner: jobLabel(job.summary.jobId),
              message: value,
            },
          ]
        : [];
    });
  if (readError && readScope === scope)
    errors.push({
      key: "read",
      owner: label("加载预览失败", "Could not load previews"),
      message: readError,
    });
  if (!candidate && !errors.length) return null;
  // Selection excludes revision so updates follow the same job/path. Import
  // names include project and candidate ownership; changing either never shows
  // or submits the previous project's private copy name.
  const artifact = candidate?.artifact,
    selectedKey = candidate ? JSON.stringify([scope, candidate.key]) : "",
    destination =
      destinationDraft.key === selectedKey ? destinationDraft.value : "",
    identity = selectedKey + ":" + artifact?.revision;
  return (
    <details className="rc-va-ai" open>
      <summary>
        <Sparkles size={14} />
        <span>{label("当前项目 AI 预览", "Project AI previews")}</span>
        {artifact && <small>r{artifact.revision}</small>}
      </summary>
      {candidate && (
        <div className="rc-va-ai-selection">
          <CeSelect
            aria-label={label("选择 AI 资产预览", "Choose AI asset preview")}
            value={candidate.key}
            onChange={(event) =>
              setSelection({ scope, key: event.target.value })
            }
          >
            {candidates.map((value) => (
              <option key={value.key} value={value.key}>
                {value.artifact.path} · {value.owner}
              </option>
            ))}
          </CeSelect>
          {candidates.length > 1 && <small>{candidates.length}</small>}
        </div>
      )}
      {errors.map((value) => (
        <p key={value.key} className="rc-va-ai-error" role="alert">
          <strong>{value.owner}</strong>
          {value.message}
        </p>
      ))}
      {artifact && (
        <div className="rc-va-ai-content">
          <img
            src={`data:image/png;base64,${artifact.image.data}`}
            width={artifact.image.width}
            height={artifact.image.height}
            alt={
              label("AI 资产预览", "AI asset preview") + " · " + artifact.path
            }
          />
          <div>
            <strong title={artifact.path}>{artifact.path}</strong>
            <p>
              {label(
                "预览随任务更新。导入后可编辑并保存为项目文档；AI 文件修改仍需在改动审阅中应用。",
                "The preview updates with the task. Import it to edit and save a project document. Apply AI file changes in Changes and review.",
              )}
            </p>
            <input
              aria-label={label("AI 草稿导入名称", "AI draft import name")}
              placeholder={
                artifact.path.replace(/\.rh(texture|model)\.json$/, "") +
                "-copy"
              }
              value={destination}
              onChange={(event) =>
                setDestinationDraft({
                  key: selectedKey,
                  value: event.target.value,
                })
              }
            />
            <button
              disabled={
                !artifact.document || !session.c.native || !!session.c.busy
              }
              onClick={() => {
                try {
                  const file = assetPath(
                      kind,
                      destination ||
                        artifact.path.replace(
                          /\.rh(texture|model)\.json$/,
                          "",
                        ) + "-copy",
                    ),
                    document =
                      kind === "pixel"
                        ? validatePixel(artifact.document)
                        : validateModel(artifact.document);
                  if (session.files.includes(file))
                    throw new Error(
                      label(
                        "导入名称已存在，请输入新名称。",
                        "The import name exists. Choose a new name.",
                      ),
                    );
                  session.create(file, document);
                  setImported(identity);
                } catch (e) {
                  session.setError(String(e));
                }
              }}
            >
              {label("导入为可编辑文档", "Import editable document")}
            </button>
            {!artifact.document && (
              <small>
                {label(
                  "此草稿较大，预览未附带文档。应用改动后可从文档列表打开。",
                  "This draft is large, so its preview omits the document. Open it from the document list after applying changes.",
                )}
              </small>
            )}
            {imported === identity && (
              <small role="status">
                {label(
                  "已导入；文档尚未保存。",
                  "Imported; document is unsaved.",
                )}
              </small>
            )}
          </div>
        </div>
      )}
    </details>
  );
}
