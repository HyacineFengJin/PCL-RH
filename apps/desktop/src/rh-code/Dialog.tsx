/** Modal focus belongs to the workspace shell, not to individual plugins.
 * Restore the invoking control on dismissal and keep Tab inside this surface. */
import { useEffect, useRef, type ReactNode } from "react";
import { X } from "lucide-react";
import { label } from "./locale";
export function Dialog({
  title,
  onClose,
  className = "rc-dialog",
  children,
}: {
  title: string;
  onClose: () => void;
  className?: string;
  children: ReactNode;
}) {
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const previous = document.activeElement;
    if (!root.current?.contains(previous)) root.current?.focus();
    return () => {
      if (previous instanceof HTMLElement && previous.isConnected)
        previous.focus();
    };
  }, []);
  return (
    <div className="rc-modal-backdrop">
      <div
        ref={root}
        className={className}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        onKeyDown={(e) => {
          if (e.defaultPrevented) return;
          if (e.key === "Escape") {
            e.stopPropagation();
            onClose();
          }
          if (e.key !== "Tab") return;
          const controls = [
            ...(root.current?.querySelectorAll<HTMLElement>(
              'button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), a[href], [tabindex="0"]',
            ) ?? []),
          ].filter((node) => node.offsetParent !== null);
          const first = controls[0],
            last = controls.at(-1);
          if (!first) {
            e.preventDefault();
            root.current?.focus();
            return;
          }
          if (
            e.shiftKey &&
            (document.activeElement === first ||
              document.activeElement === root.current)
          ) {
            e.preventDefault();
            last?.focus();
          } else if (
            !e.shiftKey &&
            (document.activeElement === last ||
              document.activeElement === root.current)
          ) {
            e.preventDefault();
            first.focus();
          }
        }}
      >
        <header>
          <h2>{title}</h2>
          <button
            type="button"
            className="rc-icon"
            aria-label={label("关闭", "Close")}
            onClick={onClose}
          >
            <X size={18} />
          </button>
        </header>
        {children}
      </div>
    </div>
  );
}
