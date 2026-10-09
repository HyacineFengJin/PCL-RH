/** Receipt-bound explorer/editor. Registered large versions use read-only
 * chunks; explicit editing uses the existing bounded artifact/review contract.
 * Draft buffers never overwrite source, and a review remains bound to its file.
 */
import { useEffect, useRef, useState } from "react";
import { FileCode2, Folder, FolderOpen, LockKeyhole, Save } from "lucide-react";
import { t, serviceError } from "./i18n";
import type { Api } from "./types";
import type {
  MakerProject,
  ProjectCheckpoint,
  ProjectIndexPage,
  ProjectFileChunk,
  ProjectSearchPage,
} from "./experimentalProjectTypes";
import { experimentalCall as call, type JobView } from "./experimentalTypes";
import type { MakerDraft } from "./makerWorkspaceState";
import { useMakerScope } from "./useMakerScope";
import { MakerReviewDialog } from "./MakerReviewDialog";
import { MakerFileTree } from "./MakerFileTree";
type SourceFile = {
  path: string;
  sha256: string;
  content: string | null;
  binary: boolean;
};
export function MakerSource({
  api,
  native,
  project,
  point,
  source,
  draft,
  onContext,
  readOnly = false,
  initialPath,
}: {
  api: Api;
  native: boolean;
  project?: MakerProject;
  point?: ProjectCheckpoint;
  source: { jobId: string; operationId: string };
  draft: MakerDraft;
  onContext?: (path: string) => void;
  readOnly?: boolean;
  initialPath?: string;
}) {
  const key = `${project?.id ?? ""}:${project?.revision ?? ""}:${point?.id ?? ""}:${source.jobId}:${source.operationId}`;
  const scope = useMakerScope(api, native, key);
  const [index, setIndex] = useState<ProjectIndexPage | null>(null),
    [filter, setFilter] = useState(initialPath ?? "");
  const [names, setNames] = useState<string[]>([]),
    [tabs, setTabs] = useState<string[]>([]),
    [path, setPath] = useState("");
  const [file, setFile] = useState<SourceFile | null>(null),
    [chunk, setChunk] = useState<ProjectFileChunk | null>(null),
    [text, setText] = useState("");
  const [review, setReview] = useState<{
    reviewId: string;
    reviewDigest?: string;
  } | null>(null);
  const [query, setQuery] = useState(""),
    [search, setSearch] = useState<ProjectSearchPage | null>(null);
  const [sourceWorkflow, setSourceWorkflow] = useState<string>(
      point?.workflow ?? "",
    ),
    [notice, setNotice] = useState("");
  const knownFiles = useRef(new Map<string, string>());
  const ref =
    project && point
      ? {
          id: project.id,
          expectedRevision: project.revision,
          checkpointId: point.id,
        }
      : null;
  const bufferKey = `${key}:${path}`;
  async function list(offset = 0, current = scope.readTicket("index")) {
    if (ref) {
      const value = await call<ProjectIndexPage>(api, "project_files", {
        ...ref,
        offset,
        filter,
      });
      if (!current()) return;
      if (value.fingerprint !== point!.fingerprint)
        throw new Error(t("maker.responseMismatch"));
      for (const file of value.files)
        knownFiles.current.set(file.path, file.sha256);
      setIndex(value);
      setNames(value.files.map((value) => value.path));
      if (
        initialPath &&
        !path &&
        value.files.some((file) => file.path === initialPath)
      ) {
        setPath(initialPath);
        setTabs([initialPath]);
      }
    } else {
      const value = await call<JobView>(api, "job_read", {
        jobId: source.jobId,
      });
      if (!current()) return;
      const selected = value.artifacts.find(
        (value) => value.operationId === source.operationId,
      );
      if (value.summary.jobId !== source.jobId || !selected)
        throw new Error(t("maker.responseMismatch"));
      setSourceWorkflow(value.summary.workflow);
      const filtered = selected.files.filter((name) => name.includes(filter));
      setNames(filtered.slice(offset, offset + 50));
      if (initialPath && !path && filtered.includes(initialPath)) {
        setPath(initialPath);
        setTabs([initialPath]);
      }
      setIndex({
        fingerprint: "",
        fileCount: selected.files.length,
        filteredCount: selected.files.filter((name) => name.includes(filter))
          .length,
        totalBytes: 0,
        editableSnapshot: !readOnly && value.summary.workflow === "maker",
        files: [],
        nextOffset: offset + 50 < filtered.length ? offset + 50 : null,
      });
    }
  }
  const [offset, setOffset] = useState(0);
  useEffect(() => {
    knownFiles.current.clear();
    setIndex(null);
    setNames([]);
    setTabs([]);
    setPath("");
    setFile(null);
    setChunk(null);
    setText("");
    setReview(null);
    setOffset(0);
    setSearch(null);
    setNotice("");
    setSourceWorkflow(point?.workflow ?? "");
    const current = scope.readTicket("index");
    if (native)
      void list(0, current).catch((error) => {
        if (current()) scope.setError(serviceError(error));
      });
  }, [scope.scope]);
  useEffect(() => {
    setFile(null);
    setChunk(null);
    setText("");
    setReview(null);
    if (!native || !path || (ref && !knownFiles.current.has(path))) return;
    const current = scope.readTicket("file");
    async function read() {
      if (ref) {
        const value = await call<ProjectFileChunk>(api, "project_file_read", {
          ...ref,
          path,
          offset: 0,
        });
        if (!current()) return;
        if (
          value.path !== path ||
          value.sha256 !== knownFiles.current.get(path)
        )
          throw new Error(t("maker.responseMismatch"));
        setChunk(value);
        setText(value.content);
      } else {
        const value = await call<SourceFile>(api, "artifact_read", {
          ...source,
          path,
        });
        if (!current()) return;
        if (value.path !== path) throw new Error(t("maker.responseMismatch"));
        const saved = readOnly ? undefined : draft.buffers.get(bufferKey);
        if (saved && saved.sha256 !== value.sha256)
          throw new Error(t("maker.bufferSourceChanged"));
        setFile(value);
        setText(saved?.text ?? value.content ?? "");
      }
    }
    void read().catch((error) => {
      if (current()) scope.setError(serviceError(error));
    });
  }, [scope.scope, path]);
  function choose(name: string) {
    scope.change(() => {
      setPath(name);
      setReview(null);
      setTabs((old) =>
        [...old.filter((value) => value !== name), name].slice(-8),
      );
      onContext?.(name);
    });
  }
  const editablePath =
    (path.startsWith("src/main/java/") && path.endsWith(".java")) ||
    (path.startsWith("src/main/resources/") &&
      /\.(json|mcmeta|txt|lang)$/.test(path) &&
      !path.endsWith("/fabric.mod.json"));
  const canEdit =
    !readOnly &&
    !project?.archived &&
    sourceWorkflow === "maker" &&
    !!index?.editableSnapshot &&
    editablePath &&
    !!file &&
    !file.binary;
  async function editFull() {
    const value = await call<SourceFile>(api, "artifact_read", {
      ...source,
      path,
    });
    if (!scope.responseCurrent()) return;
    if (value.path !== path || (chunk && value.sha256 !== chunk.sha256))
      throw new Error(t("maker.responseMismatch"));
    const saved = draft.buffers.get(bufferKey);
    if (saved && saved.sha256 !== value.sha256)
      throw new Error(t("maker.bufferSourceChanged"));
    setFile(value);
    setChunk(null);
    setText(saved?.text ?? value.content ?? "");
  }
  async function preview(kind: "edit" | "lock") {
    const value = await call<{ reviewId: string; reviewDigest?: string }>(
      api,
      "maker_review",
      {
        ...source,
        path,
        kind,
        ...(kind === "edit" ? { text, sha256: file?.sha256 } : {}),
      },
    );
    if (scope.responseCurrent()) setReview(value);
  }
  const disabled = scope.busy || !native;
  const directory = path.includes("/")
    ? path.slice(0, path.lastIndexOf("/"))
    : "";
  return (
    <div className="maker-source-workspace">
      <aside className="maker-source-explorer">
        <div className="maker-explorer-title">
          <FolderOpen size={15} />
          {t("maker.explorer")}
        </div>
        <input
          className="ce-field"
          aria-label={t("experimental.pathFilter")}
          value={filter}
          maxLength={120}
          placeholder={t("experimental.pathFilter")}
          disabled={disabled}
          onChange={(event) =>
            scope.change(() => {
              setFilter(event.target.value);
              setNames([]);
              setIndex(null);
              setOffset(0);
            })
          }
        />
        <button
          className="ce-button"
          disabled={disabled}
          onClick={() =>
            void scope.perform(async () => {
              await list();
              if (scope.responseCurrent()) setOffset(0);
            })
          }
        >
          {t("maker.search")}
        </button>
        <MakerFileTree
          names={names}
          selected={path}
          disabled={disabled}
          onChoose={choose}
        />
        <div className="maker-pagination compact">
          <button
            disabled={disabled || !offset}
            onClick={() =>
              void scope.perform(async () => {
                const next = Math.max(0, offset - 50);
                await list(next);
                if (scope.responseCurrent()) setOffset(next);
              })
            }
            aria-label={t("experimental.previousPage")}
          >
            ‹
          </button>
          <span>
            {offset + 1}–{offset + names.length}
          </span>
          <button
            disabled={disabled || index?.nextOffset == null}
            onClick={() =>
              void scope.perform(async () => {
                const next = index!.nextOffset!;
                await list(next);
                if (scope.responseCurrent()) setOffset(next);
              })
            }
            aria-label={t("experimental.nextPage")}
          >
            ›
          </button>
        </div>
        {ref && (
          <div className="maker-source-search">
            <input
              className="ce-field"
              aria-label={t("experimental.sourceSearch")}
              value={query}
              maxLength={120}
              placeholder={t("experimental.sourceSearch")}
              disabled={disabled}
              onChange={(event) =>
                scope.change(() => {
                  setQuery(event.target.value);
                  setSearch(null);
                })
              }
            />
            <button
              className="ce-button"
              disabled={disabled || !query}
              onClick={() =>
                void scope.perform(async () => {
                  const result = await call<ProjectSearchPage>(
                    api,
                    "project_search",
                    { ...ref, query, offset: 0 },
                  );
                  if (!scope.responseCurrent()) return;
                  if (result.fingerprint !== point!.fingerprint)
                    throw new Error(t("maker.responseMismatch"));
                  setSearch(result);
                })
              }
            >
              {t("maker.search")}
            </button>
            {search?.matches.map((match, index) => (
              <button
                className="maker-search-match"
                key={index}
                disabled={disabled}
                onClick={() => {
                  if (!scope.callbackCurrent()) return;
                  knownFiles.current.set(match.path, match.sha256);
                  choose(match.path);
                }}
              >
                {match.path}:{match.line}
                <small>{match.snippet}</small>
              </button>
            ))}
            {search?.truncatedFile && (
              <p className="experimental-origin">
                {t("experimental.searchTruncated", {
                  path: search.truncatedFile,
                })}
              </p>
            )}
            {search?.nextOffset != null && (
              <button
                className="ce-button"
                disabled={disabled}
                onClick={() =>
                  void scope.perform(async () => {
                    const result = await call<ProjectSearchPage>(
                      api,
                      "project_search",
                      { ...ref, query, offset: search.nextOffset },
                    );
                    if (
                      scope.responseCurrent() &&
                      result.fingerprint === point!.fingerprint
                    )
                      setSearch(result);
                  })
                }
              >
                {t("experimental.nextPage")}
              </button>
            )}
          </div>
        )}
      </aside>
      <div className="maker-editor-pane">
        <div className="maker-editor-tabs">
          {tabs.map((name) => (
            <button
              key={name}
              className={name === path ? "selected" : ""}
              onClick={() => choose(name)}
              disabled={disabled}
            >
              <FileCode2 size={13} />
              {name.split("/").at(-1)}
            </button>
          ))}
        </div>
        {path ? (
          <>
            <div className="maker-editor-toolbar">
              <span>
                <Folder size={13} />
                {directory} / {path.split("/").at(-1)}
              </span>
              <div>
                {ref &&
                  !file &&
                  index?.editableSnapshot &&
                  editablePath &&
                  !readOnly &&
                  !project?.archived &&
                  point?.workflow === "maker" && (
                    <button
                      className="ce-button"
                      disabled={disabled || !chunk}
                      onClick={() => void scope.perform(editFull)}
                    >
                      {t("maker.editFile")}
                    </button>
                  )}
                {canEdit && (
                  <>
                    <button
                      className="ce-button primary"
                      disabled={
                        disabled ||
                        text === file?.content ||
                        new TextEncoder().encode(text).length > 96000
                      }
                      onClick={() => void scope.perform(() => preview("edit"))}
                    >
                      <Save size={14} />
                      {t("maker.previewFileChanges")}
                    </button>
                    <button
                      className="ce-button"
                      disabled={disabled}
                      onClick={() => void scope.perform(() => preview("lock"))}
                    >
                      <LockKeyhole size={14} />
                      {t("experimental.lock")}
                    </button>
                  </>
                )}
              </div>
            </div>
            <textarea
              className="maker-code-editor"
              aria-label={t("maker.sourceEditor")}
              spellCheck={false}
              value={text}
              readOnly={!canEdit || disabled}
              onChange={(event) =>
                scope.change(() => {
                  if (!canEdit || !file) return;
                  const next = event.target.value;
                  const existingBytes = [...draft.buffers.entries()]
                    .filter(([name]) => name !== bufferKey)
                    .reduce(
                      (total, [, value]) =>
                        total + new TextEncoder().encode(value.text).length,
                      0,
                    );
                  if (
                    (draft.buffers.size >= 32 &&
                      !draft.buffers.has(bufferKey)) ||
                    existingBytes + new TextEncoder().encode(next).length >
                      2_000_000
                  ) {
                    scope.setError(t("maker.bufferLimit"));
                    return;
                  }
                  setText(next);
                  draft.buffers.set(bufferKey, {
                    text: next,
                    original: file.content ?? "",
                    sha256: file.sha256,
                  });
                })
              }
            />
            <div className="maker-editor-status">
              <span>
                {canEdit ? t("maker.editingDraft") : t("maker.readOnlySource")}
                {text !== (file?.content ?? chunk?.content ?? "")
                  ? ` · ${t("maker.unsaved")}`
                  : ""}
              </span>
              <span>
                UTF-8 · {text.split("\n").length} {t("maker.lines")}
              </span>
            </div>
            {chunk?.nextOffset != null && (
              <button
                className="ce-button"
                disabled={disabled}
                onClick={() =>
                  void scope.perform(async () => {
                    const value = await call<ProjectFileChunk>(
                      api,
                      "project_file_read",
                      { ...ref, path, offset: chunk.nextOffset },
                    );
                    if (!scope.responseCurrent()) return;
                    if (value.path !== path || value.sha256 !== chunk.sha256)
                      throw new Error(t("maker.responseMismatch"));
                    setChunk(value);
                    setText(value.content);
                  })
                }
              >
                {t("experimental.nextChunk")}
              </button>
            )}
            {!!chunk?.offset && (
              <button
                className="ce-button"
                disabled={disabled}
                onClick={() =>
                  void scope.perform(async () => {
                    const value = await call<ProjectFileChunk>(
                      api,
                      "project_file_read",
                      { ...ref, path, offset: 0 },
                    );
                    if (
                      scope.responseCurrent() &&
                      value.path === path &&
                      value.sha256 === chunk.sha256
                    ) {
                      setChunk(value);
                      setText(value.content);
                    }
                  })
                }
              >
                {t("maker.fileBeginning")}
              </button>
            )}
          </>
        ) : (
          <div className="maker-empty">
            <FileCode2 size={36} />
            <h3>{t("maker.chooseSource")}</h3>
            <p>{t("maker.sourceExplorerHelp")}</p>
          </div>
        )}
        {scope.error && (
          <p className="experimental-error" role="alert">
            {scope.error}
          </p>
        )}
        {notice && (
          <p className="experimental-origin" role="status">
            {notice}
          </p>
        )}
      </div>
      {review && (
        <MakerReviewDialog
          key={`${key}:${path}:${review.reviewId}`}
          api={api}
          native={native}
          jobId={source.jobId}
          reviewId={review.reviewId}
          digest={review.reviewDigest}
          onClose={() => {
            if (scope.responseCurrent()) setReview(null);
          }}
          onApplied={async () => {
            if (!scope.responseCurrent()) return;
            setReview(null);
            setNotice(t("maker.copyCreated"));
          }}
        />
      )}
    </div>
  );
}
