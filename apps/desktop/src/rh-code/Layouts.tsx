import { useEffect, useRef, useState } from "react";
import {
  Check,
  Copy,
  Download,
  Pencil,
  Plus,
  Save,
  Trash2,
  Upload,
} from "lucide-react";
import { Dialog } from "./Dialog";
import { ViewBoundary } from "./ViewBoundary";
import { useCode } from "./controller";
import {
  decodeLayoutPreset,
  encodeLayoutPreset,
  instantiateLayout,
  portableLayout,
} from "./layoutPresets";
import { label } from "./locale";
import type { Kernel } from "./kernel";
import type {
  Layout,
  LayoutPreset,
  LayoutContribution,
  Project,
} from "./types";

function BuiltinLayout({
  item,
  project,
  disabled,
  onApply,
}: {
  item: LayoutContribution;
  project: Project;
  disabled: boolean;
  onApply(layout: Layout): void;
}) {
  const layout = portableLayout(item.create(project));
  return (
    <button
      className="rc-layout-choice"
      disabled={disabled}
      onClick={() => onApply(layout)}
    >
      <span className="rc-layout-mini">
        <Thumbnail layout={layout} />
      </span>
      <span>{item.label}</span>
    </button>
  );
}

function Thumbnail({ layout }: { layout: Layout }) {
  if (layout.type === "area") return <span className="rc-layout-mini-area" />;
  const tracks = `${layout.ratio}fr ${1 - layout.ratio}fr`;
  return (
    <span
      className="rc-layout-mini-split"
      style={{
        gridTemplateColumns: layout.axis === "row" ? tracks : undefined,
        gridTemplateRows: layout.axis === "column" ? tracks : undefined,
      }}
    >
      {layout.children.map((n) => (
        <Thumbnail key={n.id} layout={n} />
      ))}
    </span>
  );
}

/** Layouts are reusable presentation recipes. Export goes through a canonical
 * projection, so project paths, conversation bindings and view data stay local. */
export function Layouts({
  kernel,
  onClose,
}: {
  kernel: Kernel;
  onClose: () => void;
}) {
  const c = useCode(),
    [name, setName] = useState(""),
    [editing, setEditing] = useState<LayoutPreset | null>(null),
    [transfer, setTransfer] = useState<"import" | "export" | null>(null),
    [text, setText] = useState(""),
    [error, setError] = useState(""),
    [working, setWorking] = useState(false),
    [copied, setCopied] = useState(false);
  const file = useRef<HTMLInputElement>(null),
    output = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    if (transfer) {
      output.current?.focus();
      output.current
        ?.closest(".rc-layout-transfer")
        ?.scrollIntoView({ block: "nearest" });
    }
  }, [transfer]);
  const presets = c.snapshot?.layoutPresets ?? [];
  const disabled = !c.native || working || !!c.busy;
  const apply = (layout: Layout) => {
    if (!c.project) return;
    try {
      c.mutateLayout(
        instantiateLayout(
          layout,
          c.project.conversations.map((t) => t.id),
        ),
      );
      onClose();
    } catch (e) {
      setError(String(e));
    }
  };
  async function perform(action: () => Promise<unknown>) {
    if (disabled) return;
    setWorking(true);
    setError("");
    try {
      await action();
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  }
  function exportLayout(layout: Layout, title: string) {
    try {
      setText(encodeLayoutPreset(title, layout));
      setTransfer("export");
      setCopied(false);
      setError("");
    } catch (e) {
      setError(String(e));
    }
  }
  return (
    <Dialog
      title={label("工作区布局", "Workspace layouts")}
      onClose={onClose}
      className="rc-dialog rc-layouts-dialog"
    >
      <div className="rc-layouts-scroll">
        <h3>{label("内置布局", "Built-in layouts")}</h3>
        <div className="rc-layout-choices">
          {[...kernel.layouts.values()].map((item) => {
            return (
              c.project && (
                <ViewBoundary key={item.id} name={item.label}>
                  <BuiltinLayout
                    item={item}
                    project={c.project}
                    disabled={disabled}
                    onApply={apply}
                  />
                </ViewBoundary>
              )
            );
          })}
        </div>
        <div className="rc-layout-section-heading">
          <h3>{label("我的布局", "My layouts")}</h3>
          <small>{presets.length} / 32</small>
          <span className="rc-spacer" />
          <button
            className="rc-button"
            onClick={() => {
              setTransfer("import");
              setText("");
              setError("");
            }}
          >
            <Upload size={14} />
            {label("导入", "Import")}
          </button>
          <button
            className="rc-button"
            onClick={() =>
              exportLayout(
                c.layout,
                name.trim() || label("我的布局", "My layout"),
              )
            }
          >
            <Download size={14} />
            {label("导出当前布局", "Export current layout")}
          </button>
        </div>
        <div className="rc-saved-layouts">
          {presets.length === 0 && (
            <p className="rc-muted">
              {label(
                "调整区域后，将布局保存到这里。",
                "Arrange your areas, then save a layout here.",
              )}
            </p>
          )}
          {presets.map((item) => (
            <div className="rc-saved-layout" key={item.id}>
              <button
                className="rc-layout-apply"
                onClick={() => apply(item.layout)}
                disabled={disabled}
              >
                <span className="rc-layout-mini">
                  <Thumbnail layout={item.layout} />
                </span>
                <span>{item.name}</span>
              </button>
              <button
                className="rc-icon"
                aria-label={
                  label("重命名布局", "Rename layout") + " · " + item.name
                }
                disabled={disabled}
                onClick={() => {
                  setEditing(item);
                  setName(item.name);
                }}
              >
                <Pencil size={14} />
              </button>
              <button
                className="rc-icon"
                aria-label={
                  label("导出布局", "Export layout") + " · " + item.name
                }
                onClick={() => exportLayout(item.layout, item.name)}
              >
                <Download size={14} />
              </button>
              <button
                className="rc-icon"
                aria-label={
                  label("删除布局", "Delete layout") + " · " + item.name
                }
                disabled={disabled}
                onClick={() =>
                  void perform(async () => {
                    await c.write("layout_preset_delete", { id: item.id });
                    if (editing?.id === item.id) {
                      setEditing(null);
                      setName("");
                    }
                  })
                }
              >
                <Trash2 size={14} />
              </button>
            </div>
          ))}
        </div>
        <form
          className="rc-layout-save"
          onSubmit={(e) => {
            e.preventDefault();
            void perform(async () => {
              await c.write("layout_preset_save", {
                ...(editing ? { id: editing.id } : {}),
                name: name.trim(),
                layout: editing?.layout ?? c.layout,
              });
              setName("");
              setEditing(null);
            });
          }}
        >
          <input
            aria-label={label("布局名称", "Layout name")}
            placeholder={label("布局名称", "Layout name")}
            value={name}
            maxLength={80}
            onChange={(e) => setName(e.target.value)}
          />
          <button
            className="rc-button purple"
            type="submit"
            disabled={
              disabled || !name.trim() || (!editing && presets.length >= 32)
            }
          >
            <Save size={14} />
            {editing
              ? label("保存名称", "Save name")
              : label("保存当前布局", "Save current layout")}
          </button>
          {editing && (
            <button
              className="rc-icon"
              type="button"
              aria-label={label("另存为新布局", "Save as a new layout")}
              onClick={() => {
                setEditing(null);
                setName("");
              }}
            >
              <Plus size={15} />
            </button>
          )}
        </form>
        {transfer && (
          <section className="rc-layout-transfer">
            <h3>
              {transfer === "export"
                ? label("布局 JSON", "Layout JSON")
                : label("导入布局", "Import layout")}
            </h3>
            <textarea
              ref={output}
              value={text}
              readOnly={transfer === "export"}
              spellCheck={false}
              aria-label={label("布局 JSON", "Layout JSON")}
              placeholder={label(
                "粘贴布局 JSON，或选择文件",
                "Paste layout JSON or choose a file",
              )}
              maxLength={262144}
              onChange={(e) => setText(e.target.value)}
            />
            <div className="rc-row">
              {transfer === "export" ? (
                <button
                  className="rc-button"
                  onClick={async () => {
                    output.current?.focus();
                    output.current?.select();
                    try {
                      await navigator.clipboard.writeText(text);
                      setCopied(true);
                    } catch {
                      setError(
                        label(
                          "已选中布局，按 Ctrl+C 复制",
                          "Layout selected. Press Ctrl+C to copy.",
                        ),
                      );
                    }
                  }}
                >
                  {copied ? <Check size={14} /> : <Copy size={14} />}
                  {copied
                    ? label("已复制", "Copied")
                    : label("复制 JSON", "Copy JSON")}
                </button>
              ) : (
                <>
                  <input
                    ref={file}
                    type="file"
                    accept=".json,application/json"
                    hidden
                    onChange={async (e) => {
                      const selected = e.target.files?.[0];
                      e.target.value = "";
                      if (!selected) return;
                      try {
                        if (selected.size > 262144)
                          throw new Error(
                            label("布局文件过大", "Layout file is too large"),
                          );
                        const contents = await selected.text();
                        decodeLayoutPreset(contents);
                        setText(contents);
                        setError("");
                      } catch (e) {
                        setError(String(e));
                      }
                    }}
                  />
                  <button
                    className="rc-button"
                    onClick={() => file.current?.click()}
                  >
                    <Upload size={14} />
                    {label("选择文件", "Choose file")}
                  </button>
                  <button
                    className="rc-button purple"
                    disabled={disabled || !text.trim() || presets.length >= 32}
                    onClick={() =>
                      void perform(async () => {
                        const value = decodeLayoutPreset(text);
                        await c.write("layout_preset_save", value);
                        setText("");
                        setTransfer(null);
                      })
                    }
                  >
                    {label("导入并保存", "Import and save")}
                  </button>
                </>
              )}
              <button className="rc-button" onClick={() => setTransfer(null)}>
                {label("收起", "Collapse")}
              </button>
            </div>
          </section>
        )}
        {error && (
          <p className="rc-error-text" role="alert">
            {error}
          </p>
        )}
      </div>
    </Dialog>
  );
}
