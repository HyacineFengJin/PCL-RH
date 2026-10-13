import { useState } from "react";
import { Save, Undo2, Redo2, Plus, FileCode2, Download } from "lucide-react";
import { CeSelect } from "../../CeSelect";
import { label } from "../locale";
import type { useAssetDocument } from "./useAssetDocument";
import { assetPath, type AssetKind } from "./documents";
import type { VisualDocument } from "../../../../../code/visual-assets.mjs";

export function AssetToolbar({
  kind,
  session,
  create,
  onExport,
}: {
  kind: AssetKind;
  session: ReturnType<typeof useAssetDocument>;
  create: () => VisualDocument;
  onExport: () => void;
}) {
  const [name, setName] = useState(""),
    [creating, setCreating] = useState(false),
    { c, path, files, buffer } = session;
  return (
    <>
      <header className="rc-va-document-bar">
        <CeSelect
          aria-label={label("打开资产文档", "Open asset document")}
          value={path}
          onChange={(event) => session.open(event.target.value)}
        >
          <option value="">{label("打开文档…", "Open document…")}</option>
          {[...new Set([...files, ...(path ? [path] : [])])].map((file) => (
            <option key={file} value={file}>
              {file}
            </option>
          ))}
        </CeSelect>
        <button
          title={label("新建文档", "New document")}
          aria-label={label("新建文档", "New document")}
          aria-expanded={creating}
          disabled={!c.native || !c.project}
          onClick={() => setCreating(!creating)}
        >
          <Plus size={15} />
          <span>{label("新建", "New")}</span>
        </button>
        <button
          title={label("保存文档", "Save document")}
          aria-label={label("保存文档", "Save document")}
          disabled={!session.dirty || session.pending || !!c.busy || !c.native}
          onClick={() => void session.save()}
        >
          <Save size={15} />
        </button>
        <button
          title={label("撤销", "Undo")}
          aria-label={label("撤销", "Undo")}
          disabled={!session.canUndo}
          onClick={session.undo}
        >
          <Undo2 size={15} />
        </button>
        <button
          title={label("重做", "Redo")}
          aria-label={label("重做", "Redo")}
          disabled={!session.canRedo}
          onClick={session.redo}
        >
          <Redo2 size={15} />
        </button>
        <button
          title={label("在源码中查看", "View source")}
          aria-label={label("在源码中查看", "View source")}
          disabled={!buffer}
          onClick={() => c.openFile(path)}
        >
          <FileCode2 size={15} />
        </button>
        <button
          title={
            kind === "pixel"
              ? label("导出 PNG", "Export PNG")
              : label("导出 Java 模型和 PNG", "Export Java model and PNG")
          }
          aria-label={label("打开导出设置", "Open export settings")}
          disabled={!session.document}
          onClick={onExport}
        >
          <Download size={15} />
          <span>{label("导出", "Export")}</span>
        </button>
      </header>
      {creating && (
        <form
          className="rc-va-new"
          onSubmit={(event) => {
            event.preventDefault();
            try {
              const file = assetPath(kind, name);
              if (files.includes(file))
                throw new Error(
                  label(
                    "同名文档已存在，请从列表打开。",
                    "This document exists. Open it from the list.",
                  ),
                );
              session.create(file, create());
              setCreating(false);
              setName("");
            } catch (e) {
              session.setError(String(e));
            }
          }}
        >
          <input
            autoFocus
            aria-label={label("新文档名称", "New document name")}
            placeholder={
              kind === "pixel" ? "textures/example" : "models/example"
            }
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
          <button type="submit" disabled={!name.trim()}>
            {label("创建", "Create")}
          </button>
          <small>
            {kind === "pixel" ? ".rhtexture.json" : ".rhmodel.json"}
          </small>
        </form>
      )}
      {session.error && (
        <p className="rc-va-error" role="alert">
          {session.error}
        </p>
      )}
      <div className="rc-va-document-status">
        <span title={path}>
          {path ||
            label("选择或创建项目资产", "Select or create a project asset")}
        </span>
        <span>
          {buffer
            ? session.dirty
              ? label("未保存", "Unsaved")
              : label("已保存", "Saved")
            : ""}
        </span>
      </div>
    </>
  );
}

/** Exports use the same revisioned native queue as document saves. Outputs are
 * project files and are never downloaded through a renderer-only URL. */
export function AssetExport({
  kind,
  session,
}: {
  kind: AssetKind;
  session: ReturnType<typeof useAssetDocument>;
}) {
  const [path, setPath] = useState(
      "src/main/resources/assets/example/" +
        (kind === "pixel"
          ? "textures/block/example.png"
          : "models/block/example.json"),
    ),
    [texturePath, setTexturePath] = useState(
      "src/main/resources/assets/example/textures/block/example.png",
    ),
    [textureResource, setTextureResource] = useState("example:block/example"),
    [result, setResult] = useState("");
  return (
    <section className="rc-va-export">
      <strong>
        {kind === "pixel"
          ? label("导出 PNG", "Export PNG")
          : label(
              "导出 Java 方块 / 物品模型",
              "Export Java block / item model",
            )}
      </strong>
      <label>
        {label("项目内输出路径", "Output path in project")}
        <input
          aria-label={label("导出路径", "Export path")}
          value={path}
          onChange={(event) => {
            setPath(event.target.value);
            setResult("");
          }}
        />
      </label>
      {kind === "model" && (
        <label>
          {label("材质资源名", "Texture resource name")}
          <input
            aria-label={label("材质资源名", "Texture resource name")}
            placeholder="example:block/example"
            value={textureResource}
            onChange={(event) => {
              setTextureResource(event.target.value);
              setResult("");
            }}
          />
        </label>
      )}
      {kind === "model" && (
        <label>
          {label("材质 PNG 输出路径", "Texture PNG output path")}
          <input
            aria-label={label("材质 PNG 输出路径", "Texture PNG output path")}
            value={texturePath}
            onChange={(event) => {
              setTexturePath(event.target.value);
              setResult("");
            }}
          />
        </label>
      )}
      <p>
        {kind === "pixel"
          ? label(
              "输出透明 PNG。已有文件会拒绝覆盖；请使用新路径。",
              "Exports a transparent PNG. Existing files are protected; use a new path.",
            )
          : label(
              "模型 JSON 与嵌入材质 PNG 一起写入项目。block / item 由模型路径决定；已有文件会拒绝覆盖。",
              "Writes model JSON and its embedded texture PNG into the project. Choose block / item in the model path. Existing files are protected.",
            )}
      </p>
      <button
        disabled={
          !session.c.native ||
          !session.document ||
          session.pending ||
          !!session.c.busy ||
          !path.trim()
        }
        onClick={() =>
          void session.work(async () => {
            const owner = session.c.projectId,
              document = session.document;
            if (!document || !owner) return;
            await session.c.write("asset_export", {
              projectId: owner,
              kind,
              path: path.trim(),
              content: JSON.stringify(document),
              ...(kind === "model"
                ? {
                    textureResource: textureResource.trim(),
                    texturePath: texturePath.trim(),
                  }
                : {}),
            });
            setResult(label("导出完成", "Export complete"));
          })
        }
      >
        <Download size={15} />
        {label("写入项目", "Export to project")}
      </button>
      {result && <span role="status">{result}</span>}
    </section>
  );
}
