import { Component, type ReactNode } from "react";
import { RotateCcw, TriangleAlert } from "lucide-react";
import { label } from "./locale";

/** A failed component must not take down sibling editors or their unsaved
 * buffers. Retrying remounts presentation; project data stays in the host. */
export class ViewBoundary extends Component<
  { name: string; children: ReactNode },
  { error: string | null; generation: number }
> {
  state = { error: null as string | null, generation: 0 };
  static getDerivedStateFromError(error: unknown) {
    return { error: String(error).slice(0, 1000) };
  }
  render() {
    if (this.state.error)
      return (
        <div className="rc-view-failure" role="alert">
          <TriangleAlert size={24} />
          <strong>{this.props.name}</strong>
          <p>
            {label("组件暂时无法显示", "This component could not be displayed")}
          </p>
          <button
            className="rc-button"
            onClick={() =>
              this.setState(({ generation }) => ({
                error: null,
                generation: generation + 1,
              }))
            }
          >
            <RotateCcw size={14} />
            {label("重新加载组件", "Reload component")}
          </button>
          <details>
            <summary>{label("错误详情", "Error details")}</summary>
            <pre>{this.state.error}</pre>
          </details>
        </div>
      );
    return (
      <div className="rc-view-mount" key={this.state.generation}>
        {this.props.children}
      </div>
    );
  }
}
