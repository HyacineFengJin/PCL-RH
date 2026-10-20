/** Present only the saved host projection. Editing a field must not make an
 * unapplied preset look like current launcher policy or live download progress. */
import { CheckCircle2, Info, TriangleAlert } from "lucide-react";
import { formatNumber, t } from "./i18n";
import type { ExtensionWidget } from "./experimentalTypes";

export function ExtensionWidgets({ widgets }: { widgets: ExtensionWidget[] }) {
  return (
    <div className="extension-widgets">
      {widgets.map((widget) => {
        if (widget.kind === "notice") {
          const Icon =
            widget.tone === "warning"
              ? TriangleAlert
              : widget.tone === "success"
                ? CheckCircle2
                : Info;
          return (
            <div
              key={widget.id}
              role="note"
              className={`extension-notice extension-notice-${widget.tone}`}
            >
              <Icon size={17} aria-hidden="true" />
              <span>{widget.text}</span>
            </div>
          );
        }
        if (widget.kind === "value") {
          const value =
            typeof widget.value === "boolean"
              ? t(
                  widget.value
                    ? "experimental.widgetYes"
                    : "experimental.widgetNo",
                )
              : typeof widget.value === "number"
                ? formatNumber(widget.value)
                : widget.value;
          return (
            <dl key={widget.id} className="extension-value">
              <dt>{widget.label}</dt>
              <dd>
                {value}
                {widget.suffix && (
                  <span className="extension-value-suffix">
                    {" "}
                    {widget.suffix}
                  </span>
                )}
              </dd>
            </dl>
          );
        }
        return (
          <div key={widget.id} className="extension-progress">
            <div>
              <span>{widget.label}</span>
              <span>
                {formatNumber(widget.value)} / {formatNumber(widget.max)}
              </span>
            </div>
            <progress
              aria-label={widget.label}
              value={widget.value}
              max={widget.max}
            />
          </div>
        );
      })}
    </div>
  );
}
