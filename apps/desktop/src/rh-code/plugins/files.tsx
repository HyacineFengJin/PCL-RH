import { FileCode2, Folder, Save, Layers } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useCode } from "../controller";
import { label } from "../locale";
import type { Area } from "../types";
function useFiles() {
  const c = useCode(),
    [files, setFiles] = useState<string[]>([]),
    [error, setError] = useState("");
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
        .then((r) => {
          if (alive) setFiles(r.files);
        })
        .catch((e) => {
          if (alive) setError(String(e));
        });
    return () => {
      alive = false;
    };
  }, [c.api, c.native, c.projectId, c.filesEpoch]);
  return { files, error };
}
type Branch = { files: string[]; folders: Map<string, Branch> };
function fileTree(files: string[]) {
  const root: Branch = { files: [], folders: new Map() };
  for (const file of files) {
    let node = root;
    const parts = file.split("/");
    for (const name of parts.slice(0, -1)) {
      if (!node.folders.has(name))
        node.folders.set(name, { files: [], folders: new Map() });
      node = node.folders.get(name)!;
    }
    node.files.push(file);
  }
  return root;
}
function Tree({ node, open }: { node: Branch; open: (file: string) => void }) {
  return (
    <>
      {[...node.folders]
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([name, child]) => (
          <details className="rc-file-folder" key={name} open>
            <summary>
              <Folder size={14} />
              <span>{name}</span>
            </summary>
            <div>
              <Tree node={child} open={open} />
            </div>
          </details>
        ))}
      {node.files.map((file) => (
        <button key={file} title={file} onClick={() => open(file)}>
          <FileCode2 size={14} />
          <span>{file.split("/").at(-1)}</span>
        </button>
      ))}
    </>
  );
}
export function Files() {
  const c = useCode(),
    { files, error } = useFiles();
  const tree = useMemo(() => fileTree(files), [files]);
  return (
    <div className="rc-panel-pad">
      {error && (
        <p role="alert" className="rc-error-text">
          {error}
        </p>
      )}
      <div className="rc-file-list">
        <Tree node={tree} open={c.openFile} />
      </div>
      {!files.length && !error && (
        <p className="rc-muted">
          {label("文件夹中还没有源码", "No source files in this folder yet")}
        </p>
      )}
    </div>
  );
}
export function Graph() {
  const c = useCode(),
    { files, error } = useFiles();
  const groups = new Map<string, string[]>();
  for (const f of files) {
    const key = f.includes("/assets/")
      ? label("模型与材质", "Models and textures")
      : f.includes("/data/")
        ? label("配方与数据", "Recipes and data")
        : f.endsWith(".java")
          ? label("行为与系统", "Behavior and systems")
          : label("项目配置", "Project configuration");
    groups.set(key, [...(groups.get(key) ?? []), f]);
  }
  return (
    <div className="rc-panel-pad">
      {error && <p className="rc-error-text">{error}</p>}
      {[...groups].map(([name, items]) => (
        <details className="rc-graph-group" key={name} open>
          <summary>
            <Layers size={16} />
            {name}
            <span>{items.length}</span>
          </summary>
          {items.map((file) => (
            <button key={file} onClick={() => c.openFile(file)}>
              {file.split("/").at(-1)}
            </button>
          ))}
        </details>
      ))}
      {!files.length && !error && (
        <div className="rc-muted">
          {label(
            "内容将随项目源码显示",
            "Project content appears here as files are added",
          )}
        </div>
      )}
    </div>
  );
}
export function Source({ area }: { area: Area }) {
  const c = useCode(),
    file = area.file,
    [error, setError] = useState("");
  useEffect(() => {
    let alive = true;
    setError("");
    if (file && c.native)
      void c.loadFile(file).catch((e) => {
        if (alive) setError(String(e));
      });
    return () => {
      alive = false;
    };
  }, [file, c.projectId, c.native, c.filesEpoch]);
  const value = file && c.buffer(file);
  if (!file)
    return (
      <div className="rc-chat-empty">
        <FileCode2 size={28} />
        <p>{label("从文件图谱打开源码", "Open source from the file tree")}</p>
      </div>
    );
  return (
    <div className="rc-source">
      <header>
        <span>{file}</span>
        <span className="rc-spacer" />
        <button
          className="rc-icon"
          aria-label={label("保存源码", "Save source")}
          disabled={
            !value || value.content === value.original || !!c.busy || !c.native
          }
          onClick={() =>
            void c.saveFile(file).catch((e) => setError(String(e)))
          }
        >
          <Save size={16} />
        </button>
      </header>
      {error && (
        <p className="rc-error-text" role="alert">
          {error}
        </p>
      )}
      <textarea
        aria-label={label("源码缓冲区", "Source buffer") + " · " + file}
        value={value ? value.content : ""}
        disabled={!value || !c.native}
        spellCheck={false}
        onChange={(e) => c.edit(file, e.target.value)}
      />
      <footer>
        {value && value.content !== value.original
          ? label("未保存", "Unsaved")
          : label("已与磁盘同步", "Matches disk")}
      </footer>
    </div>
  );
}
