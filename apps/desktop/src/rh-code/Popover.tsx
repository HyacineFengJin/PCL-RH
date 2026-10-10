/** Portals keep an area's controls usable even in a narrow split. The popup
 * owns outside-click and keyboard handlers only while it is open. */
import { useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
export function Popover({
  label,
  icon,
  children,
}: {
  label: string;
  icon: ReactNode;
  children: ReactNode;
}) {
  const trigger = useRef<HTMLButtonElement>(null),
    popup = useRef<HTMLDivElement>(null),
    [position, setPosition] = useState<{ top: number; left: number } | null>(
      null,
    );
  useEffect(() => {
    if (!position) return;
    const close = (event: PointerEvent) => {
      if (
        !popup.current?.contains(event.target as Node) &&
        !trigger.current?.contains(event.target as Node)
      )
        setPosition(null);
    };
    const key = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setPosition(null);
        trigger.current?.focus();
      }
    };
    const retire = () => setPosition(null);
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", key);
    window.addEventListener("resize", retire);
    return () => {
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", key);
      window.removeEventListener("resize", retire);
    };
  }, [position]);
  return (
    <>
      <button
        ref={trigger}
        className="rc-icon"
        aria-label={label}
        aria-expanded={!!position}
        onClick={() => {
          if (position) setPosition(null);
          else {
            const rect = trigger.current!.getBoundingClientRect();
            setPosition({
              top: Math.min(rect.bottom + 4, window.innerHeight - 230),
              left: Math.max(
                8,
                Math.min(rect.right - 210, window.innerWidth - 220),
              ),
            });
          }
        }}
      >
        {icon}
      </button>
      {position &&
        createPortal(
          <div
            className="rh-code rc-popover-shell"
            ref={popup}
            style={{ top: position.top, left: position.left }}
          >
            <div className="rc-menu" onClick={() => setPosition(null)}>
              {children}
            </div>
          </div>,
          document.body,
        )}
    </>
  );
}
