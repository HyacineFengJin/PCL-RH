import { useEffect, useState } from "react";
import { Box, Import } from "lucide-react";
import { CeSelect } from "../../CeSelect";
import {
  createModel,
  editModel,
  validateModel,
  validatePixel,
  type ModelDocument,
  type ModelOperation,
} from "../../../../../code/visual-assets.mjs";
import { label } from "../locale";
import type { ViewProps } from "../types";
import { AssetToolbar, AssetExport } from "./AssetToolbar";
import { AiAssetPreview } from "./AiAssetPreview";
import { resizeTexture } from "./documents";
import { TexturePanel } from "./TexturePanel";
import { ModelPreview } from "./ModelPreview";
import { ModelGeometry } from "./ModelGeometry";
import { useAssetDocument } from "./useAssetDocument";
import "./visual-assets.css";

export function ModelEditor(props: ViewProps) {
  const session = useAssetDocument("model", props),
    [exporting, setExporting] = useState(false),
    [selected, setSelected] = useState(""),
    [textures, setTextures] = useState<string[]>([]),
    [texturePath, setTexturePath] = useState(""),
    document = session.document as ModelDocument | undefined;
  useEffect(() => {
    let alive = true;
    setTextures([]);
    setTexturePath("");
    if (session.c.native && session.c.project)
      void session.c
        .api<{ files: string[] }>("rh_code_call", {
          operation: "files",
          args: { projectId: session.c.projectId },
        })
        .then((value) => {
          if (alive)
            setTextures(
              value.files.filter((path) => path.endsWith(".rhtexture.json")),
            );
        })
        .catch((e) => {
          if (alive) session.setError(String(e));
        });
    return () => {
      alive = false;
    };
  }, [
    session.c.api,
    session.c.native,
    session.c.projectId,
    session.c.filesEpoch,
  ]);
  useEffect(() => {
    setSelected("");
  }, [session.c.projectId, session.path]);
  function edit(operations: ModelOperation[], record = true) {
    if (!document) return false;
    try {
      const current = session.buffer
        ? JSON.parse(session.buffer.content)
        : document;
      return session.change(editModel(current, operations), record);
    } catch (e) {
      session.setError(String(e));
      return false;
    }
  }
  return (
    <div className="rc-va-editor rc-va-model-editor">
      <AssetToolbar
        kind="model"
        session={session}
        create={createModel}
        onExport={() => setExporting(!exporting)}
      />
      <div className="rc-va-body">
        <AiAssetPreview kind="model" session={session} />
        {exporting && (
          <AssetExport
            key={session.c.projectId + ":" + session.path}
            kind="model"
            session={session}
          />
        )}
        {document ? (
          <>
            <div className="rc-va-model-layout">
              <ModelPreview document={document} />
              <ModelGeometry
                document={document}
                selected={selected}
                select={setSelected}
                edit={edit}
              />
            </div>
            <details className="rc-va-embedded-texture" open>
              <summary>
                {label("嵌入材质", "Embedded texture")}
                <small>
                  {document.texture.width}×{document.texture.height}
                </small>
              </summary>
              <div className="rc-va-import-row">
                <CeSelect
                  aria-label={label(
                    "导入项目材质文档",
                    "Import project texture document",
                  )}
                  value={texturePath}
                  onChange={(event) => setTexturePath(event.target.value)}
                >
                  <option value="">
                    {label("选择项目材质…", "Choose project texture…")}
                  </option>
                  {[
                    ...new Set([
                      ...textures,
                      ...session.c.assetDocumentPaths("pixel"),
                    ]),
                  ].map((path) => (
                    <option key={path} value={path}>
                      {path}
                    </option>
                  ))}
                </CeSelect>
                <button
                  disabled={!texturePath || session.pending}
                  onClick={() =>
                    void session.work(async () => {
                      const path = session.path,
                        original = session.buffer?.content,
                        textureBuffer = await session.c.loadFile(texturePath);
                      if (!textureBuffer) return;
                      // Import must not replace edits made while a file read is pending.
                      if (
                        !session.isCurrent() ||
                        session.c.buffer(path)?.content !== original
                      )
                        throw new Error(
                          label(
                            "模型已更改，请重新导入材质。",
                            "The model changed. Import the texture again.",
                          ),
                        );
                      const texture = validatePixel(
                        JSON.parse(textureBuffer.content),
                      );
                      session.change(validateModel({ ...document, texture }));
                    })
                  }
                >
                  <Import size={14} />
                  {label("导入", "Import")}
                </button>
              </div>
              <TexturePanel
                compact
                document={document.texture}
                edit={(operations, record) =>
                  edit([{ op: "texture", operations }], record)
                }
                resize={(size) => {
                  try {
                    session.change(
                      validateModel({
                        ...document,
                        texture: resizeTexture(document.texture, size),
                      }),
                    );
                  } catch (e) {
                    session.setError(String(e));
                  }
                }}
              />
            </details>
          </>
        ) : (
          <div className="rc-va-empty">
            <Box size={28} />
            <p>
              {label(
                "创建或打开由方块元素组成的 Java 模型。",
                "Create or open a Java model made of cuboids.",
              )}
            </p>
            <small>
              {label(
                "调整坐标、旋转和每面的 UV，并编辑嵌入材质。",
                "Edit coordinates, rotations, per-face UVs and the embedded texture.",
              )}
            </small>
          </div>
        )}
      </div>
    </div>
  );
}
