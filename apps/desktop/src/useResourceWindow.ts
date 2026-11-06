/** Fixed-height local resource rows share the page's scroll container. Only
 * presentation is windowed; selection and file identities stay in ResourcePanel.
 * A focused row remains mounted, and both endpoints retain native Tab entry. */
import { useLayoutEffect, useRef, useState } from "react";
import type { FocusEvent, KeyboardEvent } from "react";

export const RESOURCE_ROW_HEIGHT = 44;
const WINDOW_THRESHOLD = 200;
const OVERSCAN = 8;
const INITIAL_ROWS = 24;
type FocusRequest = { path: string; lastControl: boolean };

function scrollParent(list: HTMLElement): HTMLElement {
  for (let parent = list.parentElement; parent; parent = parent.parentElement) {
    if (
      /^(auto|scroll|overlay)$/.test(window.getComputedStyle(parent).overflowY)
    )
      return parent;
  }
  return list.ownerDocument.scrollingElement as HTMLElement;
}

function viewport(scroller: HTMLElement) {
  const rect = scroller.getBoundingClientRect();
  return { top: rect.top + scroller.clientTop, height: scroller.clientHeight };
}

function resourceRow(target: EventTarget | null, list: HTMLElement) {
  const row = (target as HTMLElement | null)?.closest<HTMLElement>(
    "[data-resource-index]",
  );
  return row && list.contains(row) ? row : null;
}

export function useResourceWindow(paths: readonly string[], enabled: boolean) {
  const listRef = useRef<HTMLDivElement>(null);
  const [range, setRange] = useState({ start: 0, end: INITIAL_ROWS });
  const [focusedPath, setFocusedPath] = useState<string | null>(null);
  const [focusRequest, setFocusRequest] = useState<FocusRequest | null>(null);
  const virtual = enabled && paths.length >= WINDOW_THRESHOLD;

  useLayoutEffect(() => {
    const list = listRef.current;
    if (!virtual || !list) return;
    const scroller = scrollParent(list);
    let frame = 0;
    let active = true;
    function measure() {
      frame = 0;
      if (!active) return;
      const view = viewport(scroller);
      const offset = view.top - list!.getBoundingClientRect().top;
      const start = Math.min(
        paths.length,
        Math.max(0, Math.floor(offset / RESOURCE_ROW_HEIGHT) - OVERSCAN),
      );
      const end = Math.min(
        paths.length,
        Math.max(
          start,
          Math.ceil((offset + view.height) / RESOURCE_ROW_HEIGHT) + OVERSCAN,
        ),
      );
      setRange((previous) =>
        previous.start === start && previous.end === end
          ? previous
          : { start, end },
      );
      // Blur can mean that the window lost focus. Keep the row in that case:
      // document.activeElement still owns it and must survive later scrolling.
      if (!list!.contains(list!.ownerDocument.activeElement))
        setFocusedPath(null);
    }
    function schedule() {
      if (active && !frame) frame = window.requestAnimationFrame(measure);
    }
    scroller.addEventListener("scroll", schedule, { passive: true });
    window.addEventListener("resize", schedule);
    list.addEventListener("focusout", schedule);
    const observer = new ResizeObserver(schedule);
    // Status messages and the toolbar can move the list without resizing the
    // viewport. Observe its layout ancestors as well as the viewport itself.
    for (let node: HTMLElement | null = list; node; node = node.parentElement) {
      observer.observe(node);
      if (node === scroller) break;
    }
    const fonts = list.ownerDocument.fonts;
    fonts?.addEventListener("loadingdone", schedule);
    measure();
    return () => {
      active = false;
      if (frame) window.cancelAnimationFrame(frame);
      observer.disconnect();
      scroller.removeEventListener("scroll", schedule);
      window.removeEventListener("resize", schedule);
      list.removeEventListener("focusout", schedule);
      fonts?.removeEventListener("loadingdone", schedule);
    };
  }, [virtual, paths]);

  useLayoutEffect(() => {
    if (!focusRequest) return;
    const list = listRef.current;
    const index = paths.indexOf(focusRequest.path);
    const row = list?.querySelector<HTMLElement>(
      `[data-resource-index="${index}"]`,
    );
    if (list && row) {
      const scroller = scrollParent(list);
      const view = viewport(scroller);
      const rect = row.getBoundingClientRect();
      if (rect.top < view.top) scroller.scrollTop += rect.top - view.top;
      else if (rect.bottom > view.top + view.height)
        scroller.scrollTop += rect.bottom - view.top - view.height;
      const controls = row.querySelectorAll<HTMLButtonElement>(
        "button:not(:disabled)",
      );
      // Hover actions are visibility:hidden until :focus-within. Focus the row
      // first, otherwise reverse Tab cannot focus an action in an unhovered row.
      row.focus({ preventScroll: true });
      if (focusRequest.lastControl)
        controls[controls.length - 1]?.focus({ preventScroll: true });
    }
    setFocusRequest(null);
  }, [focusRequest, paths]);

  function onFocusCapture(event: FocusEvent<HTMLDivElement>) {
    const row = resourceRow(event.target, event.currentTarget);
    if (row) setFocusedPath(paths[Number(row.dataset.resourceIndex)] ?? null);
  }

  function onKeyDownCapture(event: KeyboardEvent<HTMLDivElement>) {
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    const row = resourceRow(event.target, event.currentTarget);
    if (!row) return;
    const index = Number(row.dataset.resourceIndex);
    let next = index;
    let lastControl = false;
    if (event.key === "ArrowDown") next++;
    else if (event.key === "ArrowUp") next--;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = paths.length - 1;
    else if (event.key === "Tab" && virtual) {
      const controls = row.querySelectorAll<HTMLButtonElement>(
        "button:not(:disabled)",
      );
      if (event.shiftKey && event.target === row) {
        next--;
        lastControl = true;
      } else if (
        !event.shiftKey &&
        event.target === (controls[controls.length - 1] || row)
      )
        next++;
      else return;
      // The endpoints are always in DOM order, so the browser can leave the
      // list normally. Inside the list Tab follows all logical rows, not gaps.
      if (next < 0 || next >= paths.length) return;
    } else return;
    event.preventDefault();
    next = Math.max(0, Math.min(paths.length - 1, next));
    if (paths[next]) setFocusRequest({ path: paths[next], lastControl });
  }

  const indices: number[] = [];
  if (virtual) {
    const retained = new Set([0, paths.length - 1]);
    for (
      let index = range.start;
      index < Math.min(paths.length, range.end);
      index++
    )
      retained.add(index);
    if (focusedPath !== null) {
      const index = paths.indexOf(focusedPath);
      if (index !== -1) retained.add(index);
    }
    if (focusRequest) {
      const index = paths.indexOf(focusRequest.path);
      if (index !== -1) retained.add(index);
    }
    indices.push(...[...retained].sort((a, b) => a - b));
  } else {
    for (let index = 0; index < paths.length; index++) indices.push(index);
  }
  return {
    listRef,
    virtual,
    indices,
    onFocusCapture,
    onKeyDownCapture,
  };
}
