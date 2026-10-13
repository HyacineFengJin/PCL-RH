import { useEffect, useRef, useState } from "react";
import { RotateCcw } from "lucide-react";
import {
  renderModel,
  type ModelDocument,
} from "../../../../../code/visual-assets.mjs";
import { label } from "../locale";
import { putCanvasImage } from "./TexturePanel";

/** Orthographic CPU preview renders only when geometry, texture or orbit
 * changes. It does not retain a WebGL context or run an animation loop. */
export function ModelPreview({ document }: { document: ModelDocument }) {
  const canvas = useRef<HTMLCanvasElement>(null),
    drag = useRef<{
      pointer: number;
      x: number;
      y: number;
      yaw: number;
      pitch: number;
    } | null>(null),
    [yaw, setYaw] = useState(45),
    [pitch, setPitch] = useState(30),
    [error, setError] = useState("");
  useEffect(() => {
    try {
      const size = document.elements.length > 64 ? 192 : 256;
      if (canvas.current)
        putCanvasImage(
          canvas.current,
          renderModel(document, { width: size, height: size, yaw, pitch }),
        );
      setError("");
    } catch (e) {
      setError(String(e));
    }
  }, [document, yaw, pitch]);
  function finish(event: React.PointerEvent<HTMLCanvasElement>) {
    drag.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId))
      event.currentTarget.releasePointerCapture(event.pointerId);
  }
  return (
    <section className="rc-va-model-preview">
      <div className="rc-va-preview-stage">
        <canvas
          ref={canvas}
          role="img"
          aria-label={label(
            "模型预览；拖动旋转视角",
            "Model preview; drag to orbit",
          )}
          onPointerDown={(event) => {
            if (event.button !== 0) return;
            event.preventDefault();
            event.currentTarget.setPointerCapture(event.pointerId);
            drag.current = {
              pointer: event.pointerId,
              x: event.clientX,
              y: event.clientY,
              yaw,
              pitch,
            };
          }}
          onPointerMove={(event) => {
            const start = drag.current;
            if (!start || start.pointer !== event.pointerId) return;
            setYaw((start.yaw + (event.clientX - start.x) * 0.6) % 360);
            setPitch(
              Math.max(
                -85,
                Math.min(85, start.pitch + (event.clientY - start.y) * 0.4),
              ),
            );
          }}
          onPointerUp={finish}
          onPointerCancel={finish}
          onLostPointerCapture={() => {
            drag.current = null;
          }}
        />
      </div>
      <div className="rc-va-orbit-controls">
        <label>
          {label("水平", "Yaw")}
          <input
            aria-label={label("预览水平角度", "Preview yaw")}
            type="range"
            min={-180}
            max={180}
            value={yaw > 180 ? yaw - 360 : yaw < -180 ? yaw + 360 : yaw}
            onChange={(event) => setYaw(Number(event.target.value))}
          />
        </label>
        <label>
          {label("俯仰", "Pitch")}
          <input
            aria-label={label("预览俯仰角度", "Preview pitch")}
            type="range"
            min={-85}
            max={85}
            value={pitch}
            onChange={(event) => setPitch(Number(event.target.value))}
          />
        </label>
        <button
          aria-label={label("重置预览视角", "Reset preview view")}
          title={label("重置视角", "Reset view")}
          onClick={() => {
            setYaw(45);
            setPitch(30);
          }}
        >
          <RotateCcw size={14} />
        </button>
      </div>
      {error && (
        <p role="alert" className="rc-va-error">
          {error}
        </p>
      )}
      <small className="rc-va-note">
        {label(
          "拖动旋转预览；Java 模型坐标为方块内 0–16。",
          "Drag to orbit. Java model coordinates use 0–16 inside a block.",
        )}
      </small>
    </section>
  );
}
