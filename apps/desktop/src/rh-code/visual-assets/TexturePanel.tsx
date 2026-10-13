import { useEffect, useRef, useState, type CSSProperties } from "react";
import { Paintbrush, Eraser, PaintBucket, Grid2X2 } from "lucide-react";
import { CeSelect } from "../../CeSelect";
import { label } from "../locale";
import {
  renderPixel,
  type PixelDocument,
  type PixelOperation,
  type Vec2,
} from "../../../../../code/visual-assets.mjs";

export function putCanvasImage(
  canvas: HTMLCanvasElement,
  value: ReturnType<typeof renderPixel>,
) {
  canvas.width = value.width;
  canvas.height = value.height;
  const context = canvas.getContext("2d");
  if (!context) return;
  const image = context.createImageData(value.width, value.height);
  image.data.set(value.data);
  context.putImageData(image, 0, 0);
}

/** Pointer gestures apply small bounded edits as they arrive. The first edit
 * records the pre-stroke document, so one undo removes the whole stroke. */
export function TexturePanel({
  document,
  edit,
  resize,
  compact = false,
}: {
  document: PixelDocument;
  edit: (operations: PixelOperation[], record: boolean) => boolean;
  resize: (size: number) => void;
  compact?: boolean;
}) {
  const [tool, setTool] = useState<"brush" | "eraser" | "fill">("brush"),
    [color, setColor] = useState(1),
    [colorValue, setColorValue] = useState("#9673cc"),
    [alpha, setAlpha] = useState(255),
    [grid, setGrid] = useState(true),
    [zoom, setZoom] = useState(compact ? 8 : 16),
    canvas = useRef<HTMLCanvasElement>(null),
    stroke = useRef<{ point: Vec2; pointer: number; recorded: boolean } | null>(
      null,
    ),
    selected = color < document.palette.length ? color : 0;
  useEffect(() => {
    if (canvas.current) putCanvasImage(canvas.current, renderPixel(document));
  }, [document]);
  useEffect(() => {
    const value = document.palette[selected];
    if (selected && value) {
      setColorValue(value.slice(0, 7));
      setAlpha(parseInt(value.slice(7, 9), 16));
    }
  }, [selected, document.palette[selected]]);
  function point(event: React.PointerEvent<HTMLCanvasElement>): Vec2 {
    const box = event.currentTarget.getBoundingClientRect();
    return [
      Math.max(
        0,
        Math.min(
          document.width - 1,
          Math.floor(((event.clientX - box.left) * document.width) / box.width),
        ),
      ),
      Math.max(
        0,
        Math.min(
          document.height - 1,
          Math.floor(
            ((event.clientY - box.top) * document.height) / box.height,
          ),
        ),
      ),
    ];
  }
  function finish(event: React.PointerEvent<HTMLCanvasElement>) {
    stroke.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId))
      event.currentTarget.releasePointerCapture(event.pointerId);
  }
  const paintColor = tool === "eraser" ? 0 : selected;
  return (
    <section className={"rc-va-texture " + (compact ? "rc-va-compact" : "")}>
      <div
        className="rc-va-tool-row"
        aria-label={label("材质工具", "Texture tools")}
      >
        {(
          [
            ["brush", Paintbrush, label("画笔", "Brush")],
            ["eraser", Eraser, label("橡皮", "Eraser")],
            ["fill", PaintBucket, label("填充", "Fill")],
          ] as const
        ).map(([id, Icon, title]) => (
          <button
            key={id}
            aria-label={title}
            title={title}
            aria-pressed={tool === id}
            onClick={() => setTool(id)}
          >
            <Icon size={15} />
            <span>{title}</span>
          </button>
        ))}
        <button
          aria-label={label("显示网格", "Show grid")}
          title={label("网格", "Grid")}
          aria-pressed={grid}
          onClick={() => setGrid(!grid)}
        >
          <Grid2X2 size={15} />
        </button>
        <label>
          {label("缩放", "Zoom")}
          <CeSelect
            aria-label={label("材质缩放", "Texture zoom")}
            value={zoom}
            onChange={(event) => setZoom(Number(event.target.value))}
          >
            {[2, 4, 8, 16, 24, 32].map((value) => (
              <option key={value} value={value}>
                {value}×
              </option>
            ))}
          </CeSelect>
        </label>
        <label>
          {label("尺寸", "Size")}
          <CeSelect
            aria-label={label("材质尺寸", "Texture size")}
            value={
              document.width === document.height ? document.width : "custom"
            }
            onChange={(event) => resize(Number(event.target.value))}
          >
            {document.width !== document.height && (
              <option value="custom">
                {document.width}×{document.height}
              </option>
            )}
            {[16, 32, 64, 128].map((value) => (
              <option key={value} value={value}>
                {value}×{value}
              </option>
            ))}
          </CeSelect>
        </label>
      </div>
      <div className="rc-va-pixel-stage">
        <div
          className={
            "rc-va-pixel-surface " + (grid && zoom >= 6 ? "rc-va-grid" : "")
          }
          style={
            {
              width: document.width * zoom,
              height: document.height * zoom,
              "--va-pixel": zoom + "px",
            } as CSSProperties
          }
        >
          <canvas
            ref={canvas}
            role="img"
            aria-label={label(
              "像素画布；选择工具后拖动绘制",
              "Pixel canvas; choose a tool and drag to paint",
            )}
            style={{ width: "100%", height: "100%" }}
            onPointerDown={(event) => {
              if (event.button !== 0) return;
              event.preventDefault();
              const current = point(event);
              if (tool === "fill") {
                edit(
                  [
                    {
                      op: "fill",
                      x: current[0],
                      y: current[1],
                      color: paintColor,
                    },
                  ],
                  true,
                );
                return;
              }
              event.currentTarget.setPointerCapture(event.pointerId);
              const recorded = edit(
                [{ op: "brush", points: [current], color: paintColor }],
                true,
              );
              stroke.current = {
                point: current,
                pointer: event.pointerId,
                recorded,
              };
            }}
            onPointerMove={(event) => {
              const previous = stroke.current;
              if (!previous || previous.pointer !== event.pointerId) return;
              const current = point(event);
              if (
                current[0] === previous.point[0] &&
                current[1] === previous.point[1]
              )
                return;
              if (
                edit(
                  [
                    {
                      op: "line",
                      from: previous.point,
                      to: current,
                      color: paintColor,
                    },
                  ],
                  !previous.recorded,
                )
              )
                previous.recorded = true;
              previous.point = current;
            }}
            onPointerUp={finish}
            onPointerCancel={finish}
            onLostPointerCapture={() => {
              stroke.current = null;
            }}
          />
        </div>
      </div>
      <div
        className="rc-va-palette"
        aria-label={label("颜色调色板", "Color palette")}
      >
        {document.palette.map((value, index) => (
          <button
            key={index}
            className="rc-va-swatch"
            title={`${index} · ${value}`}
            aria-label={
              index === 0
                ? label("透明色", "Transparent")
                : `${label("颜色", "Color")} ${index} ${value}`
            }
            aria-pressed={selected === index}
            style={{ "--va-swatch": value } as CSSProperties}
            onClick={() => setColor(index)}
          >
            <span />
          </button>
        ))}
      </div>
      <div className="rc-va-color-row">
        <input
          type="color"
          aria-label={label("选择颜色", "Pick color")}
          value={colorValue}
          onChange={(event) => setColorValue(event.target.value)}
        />
        <label>
          {label("不透明度", "Opacity")}
          <input
            type="number"
            min={0}
            max={255}
            step={1}
            value={alpha}
            onChange={(event) =>
              setAlpha(
                Math.max(
                  0,
                  Math.min(255, Math.round(Number(event.target.value))),
                ),
              )
            }
          />
        </label>
        <button
          disabled={!selected}
          onClick={() =>
            edit(
              [
                {
                  op: "palette",
                  index: selected,
                  color: colorValue + alpha.toString(16).padStart(2, "0"),
                },
              ],
              true,
            )
          }
        >
          {label("修改颜色", "Update color")}
        </button>
        <button
          disabled={document.palette.length >= 256}
          onClick={() => {
            const index = document.palette.length;
            edit(
              [
                {
                  op: "palette",
                  index,
                  color: colorValue + alpha.toString(16).padStart(2, "0"),
                },
              ],
              true,
            );
            setColor(index);
          }}
        >
          {label("添加颜色", "Add color")}
        </button>
      </div>
      <small className="rc-va-note">
        {label(
          "透明色固定为 0；调整尺寸使用最近邻采样。",
          "Color 0 stays transparent. Resizing uses nearest neighbour sampling.",
        )}
      </small>
    </section>
  );
}
