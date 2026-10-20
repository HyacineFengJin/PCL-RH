/** Areas own presentation only. Resizing and replacing the view preserve the
 * project service, file buffers and conversations held outside this tree. */
import { useRef, useState, type ComponentType } from "react";
import { useMountRetention } from "./retention";
import {
  Columns2,
  Rows2,
  Maximize2,
  Minimize2,
  X,
  MoreHorizontal,
  ArrowLeftRight,
  Puzzle,
  Settings2,
} from "lucide-react";
import { Popover } from "./Popover";
import { CeSelect } from "../CeSelect";
import { Dialog } from "./Dialog";
import { ViewBoundary } from "./ViewBoundary";
import { useCode } from "./controller";
import { exchange, leaves, remove, replace, resize, split } from "./layout";
import { label } from "./locale";
import type { Kernel } from "./kernel";
import type { Area, Layout, ViewProps } from "./types";
function MountedView({
  area,
  kernel,
  View,
}: {
  area: Area;
  kernel: Kernel;
  View: ComponentType<ViewProps>;
}) {
  const c = useCode(),
    keepAwake = useMountRetention();
  return (
    <View
      area={area}
      api={{
        project: c.project,
        state: area.state?.[area.view] ?? {},
        keepAwake,
        updateState: (next) => c.updateAreaState(area.id, area.view, next),
        invoke: (id, ...args) => kernel.invoke(id, ...args),
      }}
    />
  );
}
export function Workspace({ kernel }: { kernel: Kernel }) {
  const c = useCode(),
    [maximized, setMaximized] = useState<string | null>(null),
    [settingsArea, setSettingsArea] = useState<string | null>(null);
  const panes = leaves(c.layout),
    focused = panes.find((p) => p.id === maximized);
  const contributions = kernel.listViews();
  const nameOf = (id: string) =>
    contributions.find((v) => v.id === id)?.label ??
    (id.startsWith("rh.extensions.panel.")
      ? label("插件面板暂不可用", "Plugin panel unavailable")
      : id);
  const settingsNode = panes.find((n) => n.id === settingsArea),
    Settings = settingsNode && kernel.views.get(settingsNode.view)?.settings;
  function pane(n: Area) {
    const view = kernel.views.get(n.view),
      View = view?.render;
    const siblings = panes.filter((p) => p.id !== n.id);
    return (
      <section
        className="rc-area"
        key={n.id}
        data-area={n.id}
        data-tone={view?.tone ?? "neutral"}
      >
        <header className="rc-area-header">
          {view && <view.icon size={15} />}
          <CeSelect
            aria-label={label("区域内容", "Area content")}
            value={n.view}
            onChange={(e) => c.patchArea(n.id, { view: e.target.value })}
          >
            {!contributions.some((v) => v.id === n.view) && (
              <option value={n.view} disabled>
                {nameOf(n.view)}
              </option>
            )}
            {contributions.map((v) => (
              <option
                key={v.id}
                value={v.id}
                disabled={!kernel.views.has(v.id)}
              >
                {v.label}
              </option>
            ))}
          </CeSelect>
          <span className="rc-spacer" />
          {view?.settings && (
            <button
              className="rc-icon"
              aria-label={label("组件设置", "Component settings")}
              onClick={() => setSettingsArea(n.id)}
            >
              <Settings2 size={14} />
            </button>
          )}
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
                  replace(
                    c.layout,
                    n.id,
                    split("row", n, {
                      ...structuredClone(n),
                      id: crypto.randomUUID(),
                    }),
                  ),
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
                  replace(
                    c.layout,
                    n.id,
                    split("column", n, {
                      ...structuredClone(n),
                      id: crypto.randomUUID(),
                    }),
                  ),
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
                {nameOf(other.view)}
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
            <ViewBoundary key={n.view} name={view.label}>
              <MountedView area={n} kernel={kernel} View={View} />
            </ViewBoundary>
          ) : (
            <div className="rc-chat-empty">
              <Puzzle size={25} />
              <p>
                {label(
                  "此组件暂不可用，布局已保留",
                  "This component is unavailable. Its layout is retained.",
                )}
              </p>
              <small>{nameOf(n.view)}</small>
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
    <>
      <div className="rc-workspace">
        {focused ? pane(focused) : tree(c.layout)}
      </div>
      {settingsNode && Settings && (
        <Dialog
          title={nameOf(settingsNode.view)}
          onClose={() => setSettingsArea(null)}
        >
          <ViewBoundary
            key={settingsNode.view + settingsNode.id}
            name={nameOf(settingsNode.view)}
          >
            <MountedView area={settingsNode} kernel={kernel} View={Settings} />
          </ViewBoundary>
        </Dialog>
      )}
    </>
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
