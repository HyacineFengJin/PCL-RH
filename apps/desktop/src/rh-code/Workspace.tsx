/** Areas own presentation only. Resizing and replacing the view preserve the
 * project service, file buffers and conversations held outside this tree. */
import { useRef, useState } from "react";
import {
  Columns2,
  Rows2,
  Maximize2,
  Minimize2,
  X,
  MoreHorizontal,
  ArrowLeftRight,
  Puzzle,
} from "lucide-react";
import { Popover } from "./Popover";
import { CeSelect } from "../CeSelect";
import { useCode } from "./controller";
import {
  area,
  exchange,
  leaves,
  remove,
  replace,
  resize,
  split,
} from "./layout";
import { label, viewNames } from "./locale";
import type { Kernel } from "./kernel";
import type { Area, Layout, ViewId } from "./types";
export function Workspace({ kernel }: { kernel: Kernel }) {
  const c = useCode(),
    [maximized, setMaximized] = useState<string | null>(null);
  const panes = leaves(c.layout),
    focused = panes.find((p) => p.id === maximized);
  function pane(n: Area) {
    const view = kernel.views.get(n.view),
      View = view?.render;
    const siblings = panes.filter((p) => p.id !== n.id);
    return (
      <section className="rc-area" key={n.id} data-area={n.id}>
        <header className="rc-area-header">
          {view && <view.icon size={15} />}
          <CeSelect
            aria-label={label("区域内容", "Area content")}
            value={n.view}
            onChange={(e) =>
              c.patchArea(n.id, { view: e.target.value as ViewId })
            }
          >
            {Object.keys(viewNames).map((id) => (
              <option key={id} value={id} disabled={!kernel.views.has(id)}>
                {label(...viewNames[id as ViewId])}
              </option>
            ))}
          </CeSelect>
          <span className="rc-spacer" />
          <button
            className="rc-icon"
            aria-label={
              maximized
                ? label("还原区域", "Restore area")
                : label("最大化区域", "Maximize area")
            }
            onClick={() => setMaximized(maximized ? null : n.id)}
          >
            {maximized ? <Minimize2 size={14} /> : <Maximize2 size={14} />}
          </button>
          <Popover
            label={label("区域操作", "Area actions")}
            icon={<MoreHorizontal size={17} />}
          >
            <button
              disabled={panes.length >= 12}
              onClick={() => {
                c.mutateLayout(
                  replace(c.layout, n.id, split("row", n, area("chat"))),
                );
                setMaximized(null);
              }}
            >
              <Columns2 size={15} />
              {label("左右拆分", "Split horizontally")}
            </button>
            <button
              disabled={panes.length >= 12}
              onClick={() => {
                c.mutateLayout(
                  replace(c.layout, n.id, split("column", n, area("chat"))),
                );
                setMaximized(null);
              }}
            >
              <Rows2 size={15} />
              {label("上下拆分", "Split vertically")}
            </button>
            {siblings.map((other) => (
              <button
                key={other.id}
                onClick={() =>
                  c.mutateLayout(exchange(c.layout, n.id, other.id))
                }
              >
                <ArrowLeftRight size={15} />
                {label("交换：", "Swap: ")}
                {label(...viewNames[other.view])}
              </button>
            ))}
            <button
              disabled={panes.length <= 1}
              onClick={() => {
                const next = remove(c.layout, n.id);
                if (next) c.mutateLayout(next);
                setMaximized(null);
              }}
            >
              <X size={15} />
              {label("关闭区域", "Close area")}
            </button>
          </Popover>
        </header>
        <div className="rc-area-body">
          {View ? (
            <View area={n} />
          ) : (
            <div className="rc-chat-empty">
              <Puzzle size={25} />
              <p>
                {label("此区域的插件已停用", "This area's plugin is disabled")}
              </p>
            </div>
          )}
        </div>
      </section>
    );
  }
  function tree(n: Layout): React.ReactNode {
    return n.type === "area" ? (
      pane(n)
    ) : (
      <SplitNode
        key={n.id}
        node={n}
        onRatio={(ratio) => c.mutateLayout(resize(c.layout, n.id, ratio))}
      >
        {n.children.map(tree)}
      </SplitNode>
    );
  }
  return (
    <div className="rc-workspace">
      {focused ? pane(focused) : tree(c.layout)}
    </div>
  );
}
function SplitNode({
  node,
  children,
  onRatio,
}: {
  node: Extract<Layout, { type: "split" }>;
  children: React.ReactNode[];
  onRatio: (r: number) => void;
}) {
  const ref = useRef<HTMLDivElement>(null),
    [drag, setDrag] = useState<number | null>(null),
    ratio = drag ?? node.ratio;
  return (
    <div
      ref={ref}
      className={"rc-split " + node.axis}
      style={{
        gridTemplateColumns:
          node.axis === "row" ? `${ratio}fr 6px ${1 - ratio}fr` : undefined,
        gridTemplateRows:
          node.axis === "column" ? `${ratio}fr 6px ${1 - ratio}fr` : undefined,
      }}
    >
      {children[0]}
      <button
        className="rc-divider"
        role="separator"
        aria-label={label("调整区域尺寸", "Resize areas")}
        aria-orientation={node.axis === "row" ? "vertical" : "horizontal"}
        aria-valuemin={15}
        aria-valuemax={85}
        aria-valuenow={Math.round(ratio * 100)}
        onKeyDown={(e) => {
          if (
            ["ArrowLeft", "ArrowUp", "ArrowRight", "ArrowDown"].includes(e.key)
          ) {
            e.preventDefault();
            onRatio(
              Math.max(
                0.15,
                Math.min(
                  0.85,
                  ratio +
                    (e.key === "ArrowLeft" || e.key === "ArrowUp"
                      ? -0.025
                      : 0.025),
                ),
              ),
            );
          }
        }}
        onPointerDown={(e) => {
          e.currentTarget.setPointerCapture(e.pointerId);
          setDrag(node.ratio);
        }}
        onPointerMove={(e) => {
          if (!e.currentTarget.hasPointerCapture(e.pointerId)) return;
          const box = ref.current?.getBoundingClientRect();
          if (box)
            setDrag(
              Math.max(
                0.15,
                Math.min(
                  0.85,
                  node.axis === "row"
                    ? (e.clientX - box.left) / box.width
                    : (e.clientY - box.top) / box.height,
                ),
              ),
            );
        }}
        onPointerUp={(e) => {
          if (e.currentTarget.hasPointerCapture(e.pointerId)) {
            e.currentTarget.releasePointerCapture(e.pointerId);
            onRatio(drag ?? node.ratio);
            setDrag(null);
          }
        }}
        onPointerCancel={() => setDrag(null)}
      />
      {children[1]}
    </div>
  );
}
