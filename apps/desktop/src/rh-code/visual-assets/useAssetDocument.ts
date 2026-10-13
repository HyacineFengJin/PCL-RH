import { useEffect, useMemo, useRef, useState } from "react";
import { useCode } from "../controller";
import type { ViewProps } from "../types";
import type { VisualDocument } from "../../../../../code/visual-assets.mjs";
import {
  assetSuffix,
  parseAsset,
  serializeAsset,
  type AssetKind,
} from "./documents";

/** A view keeps a path and bounded undo history. The controller keeps every
 * document draft, including documents whose panels have been closed. */
export function useAssetDocument(kind: AssetKind, { area, api }: ViewProps) {
  const c = useCode(),
    selected = typeof api.state.path === "string" ? api.state.path : "",
    path =
      selected || (area.file?.endsWith(assetSuffix(kind)) ? area.file : ""),
    [files, setFiles] = useState<string[]>([]),
    [error, setError] = useState(""),
    [pending, setPending] = useState(false),
    history = useRef({
      key: "",
      past: [] as string[],
      future: [] as string[],
      content: undefined as string | undefined,
    }),
    currentIdentity = useRef("");
  const identity = c.projectId + ":" + path;
  currentIdentity.current = identity;
  if (history.current.key !== identity)
    history.current = {
      key: identity,
      past: [],
      future: [],
      content: undefined,
    };
  useEffect(() => {
    let alive = true;
    setFiles([]);
    setError("");
    if (c.native && c.project)
      void c
        .api<{ files: string[] }>("rh_code_call", {
          operation: "files",
          args: { projectId: c.project.id },
        })
        .then((value) => {
          if (alive)
            setFiles(
              value.files.filter((file) => file.endsWith(assetSuffix(kind))),
            );
        })
        .catch((e) => {
          if (alive) setError(String(e));
        });
    return () => {
      alive = false;
    };
  }, [c.api, c.native, c.projectId, c.filesEpoch, kind]);
  useEffect(() => {
    let alive = true;
    setError("");
    setPending(false);
    if (path && c.native)
      void c.loadFile(path).catch((e) => {
        if (alive) setError(String(e));
      });
    return () => {
      alive = false;
    };
  }, [c.projectId, c.native, c.filesEpoch, path]);
  const buffer = path ? c.buffer(path) : undefined,
    parsed = useMemo(() => {
      if (!buffer) return { document: undefined, error: "" };
      try {
        return { document: parseAsset(kind, buffer.content), error: "" };
      } catch (e) {
        return { document: undefined, error: String(e) };
      }
    }, [kind, buffer?.content]);
  function synchronizeHistory() {
    // A source editor or another visual area can edit the same owned buffer.
    // Its edit becomes the new history boundary, rather than being erased by
    // this area's earlier undo entries.
    if (
      history.current.content !== undefined &&
      history.current.content !== buffer?.content
    ) {
      history.current.past = [];
      history.current.future = [];
    }
    history.current.content = buffer?.content;
  }
  synchronizeHistory();
  function open(file: string) {
    setError("");
    api.updateState({ ...api.state, path: file });
  }
  function change(document: VisualDocument, record = true) {
    const content = serializeAsset(document);
    if (!buffer || currentIdentity.current !== identity) return false;
    synchronizeHistory();
    setError("");
    if (buffer.content === content) return false;
    if (record) {
      history.current.past.push(buffer.content);
      history.current.past = history.current.past.slice(-64);
      // A large 128px texture must not keep 64 full JSON copies per panel.
      while (
        history.current.past.length > 1 &&
        history.current.past.reduce((total, value) => total + value.length, 0) >
          4 * 1024 * 1024
      )
        history.current.past.shift();
      history.current.future = [];
    }
    history.current.content = content;
    c.edit(path, content);
    return true;
  }
  function undo() {
    synchronizeHistory();
    const value = history.current.past.pop();
    if (value === undefined || !buffer) return;
    history.current.future.push(buffer.content);
    setError("");
    history.current.content = value;
    c.edit(path, value);
  }
  function redo() {
    synchronizeHistory();
    const value = history.current.future.pop();
    if (value === undefined || !buffer) return;
    history.current.past.push(buffer.content);
    setError("");
    history.current.content = value;
    c.edit(path, value);
  }
  async function work(action: () => Promise<unknown>) {
    setPending(true);
    setError("");
    try {
      await action();
    } catch (e) {
      if (currentIdentity.current === identity) setError(String(e));
    } finally {
      if (currentIdentity.current === identity) setPending(false);
    }
  }
  function create(file: string, document: VisualDocument) {
    c.createAssetDocument(file, serializeAsset(document));
    open(file);
  }
  return {
    c,
    path,
    buffer,
    document: parsed.document,
    files: [...new Set([...files, ...c.assetDocumentPaths(kind)])],
    error: error || parsed.error,
    setError,
    open,
    change,
    undo,
    redo,
    canUndo: !!history.current.past.length,
    canRedo: !!history.current.future.length,
    dirty: !!buffer && buffer.content !== buffer.original,
    pending,
    work,
    create,
    isCurrent: () => currentIdentity.current === identity,
    save: () => work(() => c.saveFile(path)),
  };
}
