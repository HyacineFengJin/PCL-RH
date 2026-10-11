import {
  Children,
  Fragment,
  isValidElement,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type CSSProperties,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { ChevronDown } from "lucide-react";
import "./ce-select.css";

type Option = {
  value: string;
  label: string;
  disabled: boolean;
  title?: string;
};
type OptionProps = {
  value?: string | number;
  label?: string;
  disabled?: boolean;
  title?: string;
  children?: ReactNode;
};
export type CeSelectProps = Omit<
  ButtonHTMLAttributes<HTMLButtonElement>,
  "value" | "onChange" | "onClick" | "onKeyDown" | "children" | "type"
> & {
  value?: string | number;
  children: ReactNode;
  // Callers consume only the stable business value. This is deliberately not
  // presented as a full synthetic DOM change event.
  onChange?: (choice: { target: { value: string } }) => void;
};
function text(node: ReactNode): string {
  return Children.toArray(node)
    .map((child) =>
      typeof child === "string" || typeof child === "number"
        ? String(child)
        : isValidElement<{ children?: ReactNode }>(child)
          ? text(child.props.children)
          : "",
    )
    .join("");
}
function optionsFrom(children: ReactNode): Option[] {
  return Children.toArray(children).flatMap((child): Option[] => {
    if (!isValidElement<OptionProps>(child)) return [];
    if (child.type === Fragment) return optionsFrom(child.props.children);
    if (child.type !== "option") return [];
    const label = child.props.label ?? text(child.props.children);
    return [
      {
        value: String(child.props.value ?? label),
        label,
        disabled: !!child.props.disabled,
        title: child.props.title,
      },
    ];
  });
}
type Menu = { owner: symbol; active: number };
type Position = CSSProperties & {
  left: number;
  top: number;
  width: number;
  maxHeight: number;
  font: string;
  letterSpacing: string;
};

/** WebKit's native popup follows the desktop theme independently of the page.
 * One themed combobox keeps the existing option values and caller admission.
 * The body portal escapes card/scroll clipping; only its own menu gets events,
 * and a retired options/value owner cannot publish a late choice. */
export function CeSelect({
  value,
  children,
  onChange,
  className = "",
  disabled,
  title,
  ...buttonProps
}: CeSelectProps) {
  const options = optionsFrom(children);
  const selectedValue =
    value == null ? (options[0]?.value ?? "") : String(value);
  const selected = options.findIndex(
    (option) => option.value === selectedValue,
  );
  const signature = JSON.stringify([selectedValue, !!disabled, options]);
  const owner = useRef({ signature, token: Symbol() });
  if (owner.current.signature !== signature)
    owner.current = { signature, token: Symbol() };
  const token = owner.current.token;
  const latest = useRef({ options, selectedValue, disabled, onChange, token });
  latest.current = { options, selectedValue, disabled, onChange, token };
  const [menu, setMenu] = useState<Menu | null>(null);
  const [position, setPosition] = useState<Position | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const search = useRef({ text: "", at: 0 });
  const id = useId();
  const open = !!menu && menu.owner === token && !disabled;
  const enabled = options.flatMap((option, index) =>
    option.disabled ? [] : [index],
  );
  function close(restoreFocus = false) {
    setMenu(null);
    search.current = { text: "", at: 0 };
    if (restoreFocus) trigger.current?.focus({ preventScroll: true });
  }
  function show(active = selected) {
    if (disabled || !enabled.length) return;
    trigger.current?.focus({ preventScroll: true });
    setPosition(null);
    setMenu({
      owner: token,
      active: enabled.includes(active) ? active : enabled[0],
    });
  }
  function choose(index: number, captured: symbol) {
    const current = latest.current;
    const option = current.options[index];
    if (
      current.disabled ||
      current.token !== captured ||
      !option ||
      option.disabled
    )
      return;
    close(true);
    if (option.value !== current.selectedValue)
      current.onChange?.({ target: { value: option.value } });
  }
  function keyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (disabled) return;
    if (event.key === "Tab") {
      close();
      return;
    }
    if (event.key === "Escape" || (event.altKey && event.key === "ArrowUp")) {
      if (open) {
        event.preventDefault();
        event.stopPropagation();
        close(true);
      }
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      if (open && menu) choose(menu.active, menu.owner);
      else show();
      return;
    }
    if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
      event.preventDefault();
      if (!enabled.length) return;
      const current = enabled.indexOf(open && menu ? menu.active : selected);
      const next =
        event.key === "Home"
          ? enabled[0]
          : event.key === "End"
            ? enabled[enabled.length - 1]
            : !open
              ? enabled.includes(selected)
                ? selected
                : enabled[0]
              : enabled[
                  Math.max(
                    0,
                    Math.min(
                      enabled.length - 1,
                      current + (event.key === "ArrowDown" ? 1 : -1),
                    ),
                  )
                ];
      if (open) setMenu({ owner: token, active: next });
      else show(next);
      return;
    }
    if (
      event.key.length === 1 &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.altKey
    ) {
      event.preventDefault();
      const now = Date.now();
      search.current = {
        text:
          (now - search.current.at < 750 ? search.current.text : "") +
          event.key.toLocaleLowerCase(),
        at: now,
      };
      const query = /^([\s\S])\1+$/.test(search.current.text)
        ? search.current.text[0]
        : search.current.text;
      const active = open && menu ? menu.active : selected;
      const order = [
        ...enabled.filter((index) => index > active),
        ...enabled.filter((index) => index <= active),
      ];
      const found = order.find((index) =>
        options[index].label.toLocaleLowerCase().startsWith(query),
      );
      if (found != null) {
        if (open) setMenu({ owner: token, active: found });
        else show(found);
      }
    }
  }
  useLayoutEffect(() => {
    if (!open || !trigger.current) return;
    function place() {
      const button = trigger.current;
      if (!button || !button.isConnected) return close();
      const rect = button.getBoundingClientRect();
      if (
        rect.bottom <= 0 ||
        rect.top >= window.innerHeight ||
        !rect.width ||
        !rect.height
      )
        return close();
      const margin = 8,
        gap = 4;
      const width = Math.min(rect.width, window.innerWidth - margin * 2);
      const below = Math.max(
        0,
        window.innerHeight - rect.bottom - margin - gap,
      );
      const above = Math.max(0, rect.top - margin - gap);
      const downward =
        below >= Math.min(120, options.length * 30 + 8) || below >= above;
      const height = Math.min(
        320,
        options.length * 30 + 8,
        downward ? below : above,
      );
      const style = getComputedStyle(button);
      setPosition({
        // The menu lives outside the caller's subtree. Copy only its visual
        // tokens so a workspace dropdown keeps the owning surface's theme.
        ...Object.fromEntries(
          [
            "surface",
            "border",
            "active",
            "active-border",
            "text",
            "accent",
            "radius",
          ]
            .map((name) => {
              const key = `--ce-menu-${name}`;
              return [key, style.getPropertyValue(key).trim()];
            })
            .filter(([, value]) => value),
        ),
        left: Math.max(
          margin,
          Math.min(rect.left, window.innerWidth - width - margin),
        ),
        top: downward ? rect.bottom + gap : rect.top - gap - height,
        width,
        maxHeight: height,
        font: style.font,
        letterSpacing: style.letterSpacing,
      });
    }
    function outside(event: Event) {
      if (
        event.target instanceof Node &&
        !trigger.current?.contains(event.target) &&
        !list.current?.contains(event.target)
      )
        close();
    }
    function scroll(event: Event) {
      if (event.target !== list.current) place();
    }
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", scroll, true);
    document.addEventListener("pointerdown", outside, true);
    document.addEventListener("focusin", outside);
    const observer =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(place);
    observer?.observe(trigger.current!);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", scroll, true);
      document.removeEventListener("pointerdown", outside, true);
      document.removeEventListener("focusin", outside);
    };
  }, [open, token]);
  useLayoutEffect(() => {
    if (!open || !position || !menu || !list.current) return;
    const item = list.current.children[menu.active] as HTMLElement | undefined;
    if (!item) return;
    const viewport = list.current;
    if (item.offsetTop < viewport.scrollTop)
      viewport.scrollTop = item.offsetTop;
    else if (
      item.offsetTop + item.offsetHeight >
      viewport.scrollTop + viewport.clientHeight
    )
      viewport.scrollTop =
        item.offsetTop + item.offsetHeight - viewport.clientHeight;
  }, [open, position, menu?.active]);
  return (
    <>
      <button
        {...buttonProps}
        ref={trigger}
        type="button"
        role="combobox"
        className={`${className} ce-select${open ? " is-open" : ""}`}
        disabled={disabled}
        title={title || options[selected]?.title}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        aria-activedescendant={
          open && menu ? `${id}-${menu.active}` : undefined
        }
        onClick={() => (open ? close(true) : show())}
        onKeyDown={keyDown}
      >
        <span className="ce-select-value">
          {options[selected]?.label ?? selectedValue}
        </span>
        <ChevronDown size={14} className="ce-select-arrow" aria-hidden="true" />
      </button>
      {open &&
        position &&
        menu &&
        createPortal(
          <div
            ref={list}
            id={id}
            role="listbox"
            className="ce-select-menu"
            aria-label={buttonProps["aria-label"]}
            style={position}
          >
            {options.map((option, index) => (
              <div
                key={`${option.value}-${index}`}
                id={`${id}-${index}`}
                role="option"
                aria-selected={option.value === selectedValue}
                aria-disabled={option.disabled || undefined}
                title={option.title}
                className={`ce-select-option${index === menu.active ? " is-active" : ""}`}
                onPointerDown={(event) => event.preventDefault()}
                onPointerMove={() => {
                  if (!option.disabled && latest.current.token === menu.owner)
                    setMenu({ owner: menu.owner, active: index });
                }}
                onClick={() => choose(index, menu.owner)}
              >
                {option.label}
              </div>
            ))}
          </div>,
          document.body,
        )}
    </>
  );
}
