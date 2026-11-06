/** Exercise the real window hook with deterministic geometry and keyed row
 * ownership. No browser, filesystem resource inventory or native API is used. */
import assert from "node:assert/strict";
import fs from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const source = new URL("../src/useResourceWindow.ts", import.meta.url);
const code = ts.transpileModule(fs.readFileSync(source, "utf8"), {
  compilerOptions: {
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.CommonJS,
  },
}).outputText;

class EventSource {
  listeners = new Map();
  addEventListener(type, listener) {
    if (!this.listeners.has(type)) this.listeners.set(type, new Set());
    this.listeners.get(type).add(listener);
  }
  removeEventListener(type, listener) {
    this.listeners.get(type)?.delete(listener);
  }
  emit(type) {
    for (const listener of this.listeners.get(type) || []) listener();
  }
  count(type) {
    return this.listeners.get(type)?.size || 0;
  }
}

function windowHarness(count = 1000) {
  const hooks = [],
    frames = new Map(),
    observers = [];
  const document = { activeElement: null, fonts: new EventSource() };
  const scroller = Object.assign(new EventSource(), {
    clientTop: 2,
    clientHeight: 440,
    scrollTop: 0,
    parentElement: null,
    overflowY: "auto",
    getBoundingClientRect: () => ({ top: 37 }),
  });
  const panel = { parentElement: scroller, overflowY: "visible" };
  const card = { parentElement: panel, overflowY: "visible" };
  const outside = { name: "outside" };
  const list = Object.assign(new EventSource(), {
    ownerDocument: document,
    parentElement: card,
    offset: 180,
    getBoundingClientRect: () => ({
      top: 39 + list.offset - scroller.scrollTop,
    }),
    contains(target) {
      return target === list || !!target?.row?.isConnected;
    },
    querySelector(selector) {
      const index = Number(selector.match(/"(\d+)"/)[1]);
      return [...rows.values()].find(
        (row) => row.isConnected && Number(row.dataset.resourceIndex) === index,
      );
    },
  });
  document.scrollingElement = scroller;
  document.activeElement = outside;
  const browser = Object.assign(new EventSource(), {
    getComputedStyle: (node) => ({ overflowY: node.overflowY }),
    requestAnimationFrame(fn) {
      const id = ++frameId;
      frames.set(id, fn);
      return id;
    },
    cancelAnimationFrame(id) {
      frames.delete(id);
    },
  });
  class ResizeObserver {
    nodes = new Set();
    disconnected = false;
    constructor(callback) {
      this.callback = callback;
      observers.push(this);
    }
    observe(node) {
      this.nodes.add(node);
    }
    disconnect() {
      this.disconnected = true;
      this.nodes.clear();
    }
  }
  const rows = new Map();
  let paths = Array.from({ length: count }, (_, i) => `resource-${i}`);
  let enabled = true,
    frameId = 0,
    cursor = 0,
    dirty = true,
    pending = [],
    api;
  const react = {
    useRef(value) {
      return (hooks[cursor++] ??= { current: value });
    },
    useState(initial) {
      const index = cursor++;
      if (!(index in hooks)) hooks[index] = initial;
      return [
        hooks[index],
        (next) => {
          const value = typeof next === "function" ? next(hooks[index]) : next;
          if (!Object.is(value, hooks[index])) {
            hooks[index] = value;
            dirty = true;
          }
        },
      ];
    },
    useLayoutEffect(effect, dependencies) {
      const index = cursor++,
        previous = hooks[index];
      if (
        !previous ||
        dependencies.some(
          (value, i) => !Object.is(value, previous.dependencies[i]),
        )
      ) {
        hooks[index] = { dependencies };
        pending.push(() => {
          previous?.cleanup?.();
          hooks[index].cleanup = effect();
        });
      }
    },
  };
  const module = { exports: {} };
  new Function(
    "require",
    "module",
    "exports",
    "window",
    "ResizeObserver",
    code,
  )(
    (id) => (id === "react" ? react : require(id)),
    module,
    module.exports,
    browser,
    ResizeObserver,
  );
  function focus(target) {
    document.activeElement = target;
    if (list.contains(target))
      api.onFocusCapture({ target, currentTarget: list });
    else list.emit("focusout");
  }
  function createRow(path) {
    const row = {
      path,
      dataset: {},
      isConnected: true,
      get row() {
        return row;
      },
      closest: () => row,
      getBoundingClientRect() {
        const top =
          list.getBoundingClientRect().top +
          Number(row.dataset.resourceIndex) * 44;
        return { top, bottom: top + 44 };
      },
      querySelectorAll: () => row.controls.filter((button) => !button.disabled),
      focus: () => focus(row),
    };
    row.controls = Array.from({ length: 4 }, (_, index) => ({
      row,
      index,
      closest: () => row,
      focus() {
        // Production actions are visibility:hidden until their row owns focus.
        if (document.activeElement?.row === row) focus(this);
      },
    }));
    return row;
  }
  function flush() {
    for (let attempts = 0; dirty; attempts++) {
      assert.ok(attempts < 30, "The window hook settles");
      dirty = false;
      cursor = 0;
      pending = [];
      api = module.exports.useResourceWindow(paths, enabled);
      api.listRef.current = enabled ? list : null;
      const retained = new Set(
        enabled ? api.indices.map((index) => paths[index]) : [],
      );
      for (const [path, row] of rows) {
        if (!retained.has(path)) {
          row.isConnected = false;
          rows.delete(path);
          if (document.activeElement?.row === row)
            document.activeElement = outside;
        }
      }
      for (const index of enabled ? api.indices : []) {
        const path = paths[index];
        if (!rows.has(path)) rows.set(path, createRow(path));
        rows.get(path).dataset.resourceIndex = String(index);
      }
      for (const effect of pending) effect();
    }
  }
  flush();
  return {
    get indices() {
      return api.indices;
    },
    get virtual() {
      return api.virtual;
    },
    get active() {
      return document.activeElement;
    },
    get paths() {
      return paths;
    },
    row(index) {
      return rows.get(paths[index]);
    },
    focus(index, control) {
      const row = rows.get(paths[index]);
      assert.ok(row, `Row ${index} is mounted`);
      if (control !== undefined) focus(row);
      focus(control === undefined ? row : row.controls[control]);
      flush();
    },
    blur() {
      focus(outside);
      flush();
    },
    scroll(top) {
      scroller.scrollTop = top;
      scroller.emit("scroll");
    },
    key(key, shiftKey = false) {
      let prevented = false;
      api.onKeyDownCapture({
        key,
        shiftKey,
        target: document.activeElement,
        currentTarget: list,
        preventDefault() {
          prevented = true;
        },
      });
      flush();
      return prevented;
    },
    frame() {
      const pending = [...frames.values()];
      frames.clear();
      for (const callback of pending) callback();
      flush();
    },
    replace(next, available = true) {
      paths = next;
      enabled = available;
      dirty = true;
      flush();
    },
    resize(height) {
      scroller.clientHeight = height;
      browser.emit("resize");
    },
    layout(offset) {
      list.offset = offset;
      for (const observer of observers)
        if (!observer.disconnected) observer.callback();
    },
    fonts() {
      document.fonts.emit("loadingdone");
    },
    close() {
      for (const hook of hooks) hook?.cleanup?.();
    },
    scroller,
    browser,
    list,
    observers,
    document,
    frameCount: () => frames.size,
  };
}

test("the actual page scroller bounds row creation and coalesces scroll frames", () => {
  const ui = windowHarness();
  try {
    assert.ok(ui.virtual);
    assert.ok(ui.indices.length < 30);
    assert.equal(ui.indices[0], 0);
    assert.equal(ui.indices.at(-1), 999);
    assert.equal(ui.scroller.count("scroll"), 1);
    assert.deepEqual(
      [...ui.observers[0].nodes],
      [
        ui.list,
        ui.list.parentElement,
        ui.list.parentElement.parentElement,
        ui.scroller,
      ],
    );
    ui.scroll(44 * 500 + 180);
    ui.scroll(44 * 501 + 180);
    assert.equal(ui.frameCount(), 1);
    ui.frame();
    assert.deepEqual(ui.indices, [
      0,
      ...Array.from({ length: 26 }, (_, i) => 493 + i),
      999,
    ]);
    assert.equal(
      ui.active.name,
      "outside",
      "Scrolling never takes keyboard focus",
    );
    ui.resize(880);
    ui.frame();
    assert.equal(ui.indices.length, 38);
    ui.layout(180 + 44 * 20);
    ui.fonts();
    assert.equal(ui.frameCount(), 1);
    ui.frame();
    assert.equal(ui.indices[1], 473);
  } finally {
    ui.close();
  }
});

test("scrolling retains the exact focused row and its child until focus leaves", () => {
  const ui = windowHarness();
  try {
    ui.focus(2, 1);
    const row = ui.row(2),
      button = ui.active;
    ui.scroll(44 * 500 + 180);
    ui.frame();
    assert.equal(ui.row(2), row);
    assert.equal(ui.active, button);
    assert.ok(row.isConnected);
    assert.equal(ui.scroller.scrollTop, 44 * 500 + 180);
    ui.blur();
    ui.frame();
    assert.equal(ui.row(2), undefined);
    assert.equal(row.isConnected, false);
  } finally {
    ui.close();
  }
});

test("arrows and Home/End focus logical resources across window gaps", () => {
  const ui = windowHarness();
  try {
    ui.focus(0);
    assert.ok(ui.key("End"));
    assert.equal(ui.active, ui.row(999));
    assert.equal(ui.scroller.scrollTop, 180 + 44 * 1000 - 440);
    assert.ok(ui.key("ArrowUp"));
    assert.equal(ui.active, ui.row(998));
    assert.equal(ui.active.dataset.resourceIndex, "998");
    assert.ok(ui.key("Home"));
    assert.equal(ui.active, ui.row(0));
    assert.equal(ui.scroller.scrollTop, 180);
    assert.ok(ui.key("ArrowUp"));
    assert.equal(ui.active, ui.row(0));
    ui.frame();
    assert.ok(ui.indices.length < 40);
  } finally {
    ui.close();
  }
});

test("Tab visits the next logical row and reverse Tab its last enabled action", () => {
  const ui = windowHarness();
  try {
    const boundary = ui.indices.at(-2);
    assert.equal(ui.row(boundary + 1), undefined);
    ui.focus(boundary, 3);
    assert.ok(ui.key("Tab"));
    assert.equal(ui.active, ui.row(boundary + 1));
    ui.row(boundary).controls[3].disabled = true;
    assert.ok(ui.key("Tab", true));
    assert.equal(ui.active, ui.row(boundary).controls[2]);
    ui.focus(0);
    assert.equal(
      ui.key("Tab", true),
      false,
      "Native Tab can leave the first row",
    );
    assert.equal(
      ui.key("Tab"),
      false,
      "Native Tab visits actions inside a row",
    );
    ui.focus(999, 3);
    assert.equal(ui.key("Tab"), false, "Native Tab can leave the last row");
  } finally {
    ui.close();
  }
});

test("filtering, sorting, refresh and threshold changes recompute the window", () => {
  const ui = windowHarness();
  try {
    const original = ui.paths;
    ui.scroll(44 * 900 + 180);
    ui.frame();
    const reversed = [...original].reverse();
    ui.replace(reversed);
    assert.equal(ui.row(0).path, "resource-999");
    assert.equal(ui.row(999).path, "resource-0");
    const small = original.slice(0, 199);
    ui.replace(small);
    assert.equal(ui.virtual, false);
    assert.equal(ui.indices.length, 199);
    assert.equal(ui.scroller.count("scroll"), 0);
    ui.replace(original.slice(0, 200));
    assert.equal(ui.virtual, true);
    assert.ok(ui.indices.every((index) => index >= 0 && index < 200));
    ui.replace(original, false);
    assert.equal(ui.scroller.count("scroll"), 0);
    ui.scroll(0);
    ui.replace(original);
    assert.equal(ui.scroller.count("scroll"), 1);
    assert.ok(ui.indices.length < 40);
  } finally {
    ui.close();
  }
});

test("unmount releases scroll, resize, fonts, observers and pending frames", () => {
  const ui = windowHarness();
  ui.scroll(200);
  assert.equal(ui.frameCount(), 1);
  ui.close();
  assert.equal(ui.frameCount(), 0);
  assert.equal(ui.scroller.count("scroll"), 0);
  assert.equal(ui.browser.count("resize"), 0);
  assert.equal(ui.list.count("focusout"), 0);
  assert.equal(ui.document.fonts.count("loadingdone"), 0);
  assert.ok(ui.observers.every((observer) => observer.disconnected));
  // A notification already queued by the browser must not revive an old owner.
  ui.observers[0].callback();
  assert.equal(ui.frameCount(), 0);
});
