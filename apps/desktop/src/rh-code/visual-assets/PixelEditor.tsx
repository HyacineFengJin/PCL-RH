import { useState } from "react";
import { Paintbrush } from "lucide-react";
import {
  createPixel,
  editPixel,
  type PixelDocument,
} from "../../../../../code/visual-assets.mjs";
import { label } from "../locale";
import type { ViewProps } from "../types";
import { AssetToolbar, AssetExport } from "./AssetToolbar";
import { AiAssetPreview } from "./AiAssetPreview";
import { resizeTexture } from "./documents";
import { TexturePanel } from "./TexturePanel";
import { useAssetDocument } from "./useAssetDocument";
import "./visual-assets.css";

export function PixelEditor(props: ViewProps) {
  const session = useAssetDocument("pixel", props),
    [exporting, setExporting] = useState(false),
    document = session.document as PixelDocument | undefined;
  return (
    <div className="rc-va-editor rc-va-pixel-editor">
      <AssetToolbar
        kind="pixel"
        session={session}
        create={() =>
          editPixel(createPixel(), [
            { op: "palette", index: 1, color: "#9673CCFF" },
          ])
        }
        onExport={() => setExporting(!exporting)}
      />
      <div className="rc-va-body">
        <AiAssetPreview kind="pixel" session={session} />
        {exporting && (
          <AssetExport
            key={session.c.projectId + ":" + session.path}
            kind="pixel"
            session={session}
          />
        )}
        {document ? (
          <TexturePanel
            document={document}
            edit={(operations, record) => {
              try {
                const current = session.buffer
                  ? JSON.parse(session.buffer.content)
                  : document;
                return session.change(editPixel(current, operations), record);
              } catch (e) {
                session.setError(String(e));
                return false;
              }
            }}
            resize={(size) => {
              try {
                session.change(resizeTexture(document, size));
              } catch (e) {
                session.setError(String(e));
              }
            }}
          />
        ) : (
          <div className="rc-va-empty">
            <Paintbrush size={28} />
            <p>
              {label(
                "创建 16–128 像素的材质，或打开项目中的材质文档。",
                "Create a 16–128 pixel texture or open a texture document in the project.",
              )}
            </p>
            <small>
              {label(
                "画笔、橡皮、填充和调色板；保存为 .rhtexture.json，导出 PNG。",
                "Brush, eraser, fill and palette. Save .rhtexture.json and export PNG.",
              )}
            </small>
          </div>
        )}
      </div>
    </div>
  );
}
