/** Synthetic text only. Exercise the real local search helper and component
 * handlers without a native app, log files, network, accounts or game data. */
import assert from "node:assert/strict";
import fs from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";
import { button, content, elements, harness } from "./maker-test-harness.mjs";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const helperSource = fs.readFileSync(
  new URL("../src/launcherLogSearch.ts", import.meta.url),
  "utf8",
);
const helper = { exports: {} };
new Function(
  "module",
  "exports",
  ts.transpileModule(helperSource, {
    compilerOptions: {
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.CommonJS,
    },
  }).outputText,
)(helper, helper.exports);
const {
  boundLogSearchQuery,
  LOG_SEARCH_MATCH_LIMIT,
  LOG_SEARCH_QUERY_LIMIT,
  nextLogMatch,
  searchLogText,
} = helper.exports;

const fixture = (text = "ERROR error Error", extra = {}) =>
  harness("LauncherLogContent", {
    text,
    language: "zh-CN",
    region: "zh-CN",
    ...extra,
  });
const input = (ui) =>
  elements(
    ui.tree,
    (node) => node.type === "input" && node.props.type === "text",
  )[0];
const checkbox = (ui) =>
  elements(
    ui.tree,
    (node) => node.type === "input" && node.props.type === "checkbox",
  )[0];
const marks = (ui) => elements(ui.tree, (node) => node.type === "mark");
const active = (ui) =>
  marks(ui).findIndex((node) => node.props["aria-current"] === "true");
const pre = (ui) => elements(ui.tree, (node) => node.type === "pre")[0];
const status = (ui) =>
  content(elements(ui.tree, (node) => node.props?.role === "status")[0]);
async function query(ui, value) {
  input(ui).props.onChange({ currentTarget: { value } });
  await ui.flush();
}
async function click(ui, key) {
  const target = button(ui, key);
  assert.equal(target.props.disabled, false);
  target.props.onClick();
  await ui.flush();
}
function keyEvent(key, extra = {}) {
  const state = { prevented: 0, stopped: 0 };
  return {
    state,
    key,
    shiftKey: false,
    ctrlKey: false,
    altKey: false,
    metaKey: false,
    nativeEvent: { isComposing: false },
    preventDefault() {
      ++state.prevented;
    },
    stopPropagation() {
      ++state.stopped;
    },
    ...extra,
  };
}

test("empty query has no highlights, disables navigation and preserves the loaded text", () => {
  const text = "[synthetic] one\n<not markup>\n",
    ui = fixture(text);
  assert.equal(content(pre(ui)), text);
  assert.equal(marks(ui).length, 0);
  for (const key of [
    "logs.searchPrevious",
    "logs.searchNext",
    "logs.searchClear",
  ])
    assert.equal(button(ui, key).props.disabled, true);
  assert.equal(input(ui).props.maxLength, LOG_SEARCH_QUERY_LIMIT);
  assert.equal(status(ui), ui.i18n.t("logs.searchIdle"));
  assert.equal(
    input(ui).props.title,
    ui.i18n.t("logs.searchScope", { limit: "256" }),
  );
  ui.close();
});

test("regex syntax is literal and spaces/newlines remain meaningful", () => {
  const special = String.raw`.*+?^${"$"}{}()|[]\\`;
  assert.deepEqual(searchLogText(`x ${special} ${special}`, special, false), {
    matches: [
      { start: 2, end: 2 + special.length },
      { start: 3 + special.length, end: 3 + 2 * special.length },
    ],
    truncated: false,
  });
  assert.deepEqual(searchLogText("a a\na", " ", true).matches, [
    { start: 1, end: 2 },
  ]);
  assert.deepEqual(searchLogText("a a\na", "\na", true).matches, [
    { start: 3, end: 5 },
  ]);
});

test("Unicode insensitive offsets always slice the original string", () => {
  const text = "İ ERROR error 🙂𐐀𐐨 ẞß Σςσ Kk";
  assert.deepEqual(searchLogText(text, "error", false).matches, [
    { start: 2, end: 7 },
    { start: 8, end: 13 },
  ]);
  for (const [query, originals] of [
    ["𐐨", ["𐐀", "𐐨"]],
    ["ß", ["ẞ", "ß"]],
    ["σ", ["Σ", "ς", "σ"]],
    ["k", ["K", "k"]],
    ["🙂", ["🙂"]],
  ]) {
    const result = searchLogText(text, query, false);
    assert.deepEqual(
      result.matches.map(({ start, end }) => text.slice(start, end)),
      originals,
    );
  }
  assert.equal(searchLogText(text, "i", false).matches.length, 0);
  assert.deepEqual(searchLogText("ß", "ss", false).matches, []);
});

test("query limits never split a supplementary character at the boundary", () => {
  assert.equal(
    boundLogSearchQuery("x".repeat(300)).length,
    LOG_SEARCH_QUERY_LIMIT,
  );
  assert.equal(
    boundLogSearchQuery("x".repeat(255) + "🙂tail"),
    "x".repeat(255),
  );
  assert.equal(
    boundLogSearchQuery("x".repeat(254) + "🙂tail"),
    "x".repeat(254) + "🙂",
  );
  assert.equal(boundLogSearchQuery("a "), "a ");
  assert.equal(searchLogText("aaaa", "", false).matches.length, 0);
});

test("non-overlapping matches and the match cap are explicit and exact", () => {
  assert.deepEqual(searchLogText("aaaaa", "aa", true).matches, [
    { start: 0, end: 2 },
    { start: 2, end: 4 },
  ]);
  const exact = searchLogText("x".repeat(LOG_SEARCH_MATCH_LIMIT), "x", false);
  assert.equal(exact.matches.length, LOG_SEARCH_MATCH_LIMIT);
  assert.equal(exact.truncated, false);
  const capped = searchLogText(
    "x".repeat(LOG_SEARCH_MATCH_LIMIT + 1),
    "x",
    false,
  );
  assert.equal(capped.matches.length, LOG_SEARCH_MATCH_LIMIT);
  assert.equal(capped.truncated, true);
  assert.equal(nextLogMatch(0, -1, 3), 2);
  assert.equal(nextLogMatch(2, 1, 3), 0);
  assert.equal(nextLogMatch(0, 1, 0), 0);
});

test("case switch and query changes reset the current result", async () => {
  const ui = fixture();
  await query(ui, "error");
  assert.equal(marks(ui).length, 3);
  await click(ui, "logs.searchNext");
  assert.equal(active(ui), 1);
  checkbox(ui).props.onChange({ currentTarget: { checked: true } });
  await ui.flush();
  assert.deepEqual(marks(ui).map(content), ["error"]);
  assert.equal(active(ui), 0);
  await query(ui, "ERROR");
  assert.deepEqual(marks(ui).map(content), ["ERROR"]);
  assert.equal(active(ui), 0);
  ui.close();
});

test("previous/next and Enter/Shift+Enter wrap in both directions", async () => {
  const ui = fixture();
  await query(ui, "error");
  await click(ui, "logs.searchPrevious");
  assert.equal(active(ui), 2);
  await click(ui, "logs.searchNext");
  assert.equal(active(ui), 0);
  const enter = keyEvent("Enter");
  input(ui).props.onKeyDown(enter);
  await ui.flush();
  assert.equal(active(ui), 1);
  assert.deepEqual(enter.state, { prevented: 1, stopped: 1 });
  input(ui).props.onKeyDown(keyEvent("Enter", { shiftKey: true }));
  await ui.flush();
  assert.equal(active(ui), 0);
  input(ui).props.onKeyDown(keyEvent("Enter", { shiftKey: true }));
  await ui.flush();
  assert.equal(active(ui), 2);
  ui.close();
});

test("composition and modified Enter are ignored; Escape and Clear remove search", async () => {
  const ui = fixture();
  await query(ui, "error");
  for (const extra of [
    { nativeEvent: { isComposing: true } },
    { ctrlKey: true },
    { altKey: true },
    { metaKey: true },
  ]) {
    const event = keyEvent("Enter", extra);
    input(ui).props.onKeyDown(event);
    await ui.flush();
    assert.equal(active(ui), 0);
    assert.deepEqual(event.state, { prevented: 0, stopped: 0 });
  }
  ui.tree.props.onKeyDown(
    keyEvent("Escape", { nativeEvent: { isComposing: true } }),
  );
  await ui.flush();
  assert.equal(input(ui).props.value, "error");
  const escape = keyEvent("Escape");
  ui.tree.props.onKeyDown(escape);
  await ui.flush();
  assert.equal(input(ui).props.value, "");
  assert.equal(marks(ui).length, 0);
  assert.deepEqual(escape.state, { prevented: 1, stopped: 1 });
  await query(ui, "error");
  await click(ui, "logs.searchClear");
  assert.equal(input(ui).props.value, "");
  ui.close();
});

test("no-match state disables navigation without discarding the keyword", async () => {
  const ui = fixture();
  await query(ui, "absent");
  assert.equal(status(ui), ui.i18n.t("logs.searchNoMatches"));
  assert.equal(button(ui, "logs.searchNext").props.disabled, true);
  input(ui).props.onKeyDown(keyEvent("Enter"));
  await ui.flush();
  assert.equal(input(ui).props.value, "absent");
  assert.equal(marks(ui).length, 0);
  ui.close();
});

test("oversized synthetic input is bounded with an explicit truncation notice", async () => {
  const ui = fixture("x".repeat(300));
  await query(ui, "x".repeat(300));
  assert.equal(input(ui).props.value, "x".repeat(LOG_SEARCH_QUERY_LIMIT));
  assert.equal(marks(ui).length, 1);
  assert.ok(
    status(ui).includes(
      ui.i18n.t("logs.searchQueryTruncated", { limit: "256" }),
    ),
  );
  await query(ui, "x");
  assert.equal(status(ui).includes("256"), false);
  ui.close();
});

test("oversized paste reports truncation before native maxlength can hide it", async () => {
  const ui = fixture("x".repeat(300));
  await query(ui, "prefix old suffix");
  let prevented = false;
  input(ui).props.onPaste({
    currentTarget: {
      value: "prefix old suffix",
      selectionStart: 7,
      selectionEnd: 10,
    },
    clipboardData: {
      getData: (kind) => {
        assert.equal(kind, "text");
        return "x".repeat(300);
      },
    },
    preventDefault() {
      prevented = true;
    },
  });
  await ui.flush();
  assert.equal(prevented, true);
  assert.equal(input(ui).props.value, "prefix " + "x".repeat(249));
  assert.ok(
    status(ui).includes(
      ui.i18n.t("logs.searchQueryTruncated", { limit: "256" }),
    ),
  );
  ui.close();
});

test("16 MiB of repeated text has one text copy, capped DOM and no rescans on navigation", async () => {
  const text = "a".repeat(16 * 1024 * 1024),
    ui = fixture(text);
  let calls = 0;
  const original = RegExp.prototype.exec;
  RegExp.prototype.exec = function (value) {
    if (this.source === "a" && value === text) ++calls;
    return original.call(this, value);
  };
  try {
    await query(ui, "a");
    assert.equal(calls, LOG_SEARCH_MATCH_LIMIT + 1);
    assert.equal(marks(ui).length, LOG_SEARCH_MATCH_LIMIT);
    assert.equal(pre(ui).props.children.length, 2 * LOG_SEARCH_MATCH_LIMIT + 1);
    assert.equal(
      pre(ui).props.children.reduce(
        (sum, node) => sum + content(node).length,
        0,
      ),
      text.length,
    );
    assert.ok(
      status(ui).includes(
        ui.i18n.t("logs.searchMatchLimit", {
          limit: "1,000",
          minimum: "1,001",
        }),
      ),
    );
    await click(ui, "logs.searchPrevious");
    assert.equal(active(ui), LOG_SEARCH_MATCH_LIMIT - 1);
    await click(ui, "logs.searchNext");
    assert.equal(active(ui), 0);
    assert.equal(calls, LOG_SEARCH_MATCH_LIMIT + 1);
  } finally {
    RegExp.prototype.exec = original;
    ui.close();
  }
});

test("a no-hit large log completes without materializing transformed log copies", () => {
  const text = "a".repeat(16 * 1024 * 1024);
  assert.deepEqual(searchLogText(text, "a".repeat(255) + "b", false), {
    matches: [],
    truncated: false,
  });
});

test("HTML-looking log text remains React text inside marks", async () => {
  const text =
    '<img src=x onerror="synthetic()"> & <script>synthetic()</script>';
  const ui = fixture(text);
  await query(ui, "<img");
  assert.equal(content(pre(ui)), text);
  assert.deepEqual(marks(ui).map(content), ["<img"]);
  assert.equal(
    elements(
      ui.tree,
      (node) =>
        node.type === "img" ||
        node.type === "script" ||
        node.props?.dangerouslySetInnerHTML,
    ).length,
    0,
  );
  ui.close();
});

test("replaced text retires old query, case setting and retained handlers immediately", async () => {
  const ui = fixture();
  await query(ui, "error");
  checkbox(ui).props.onChange({ currentTarget: { checked: true } });
  await ui.flush();
  const oldInput = input(ui),
    oldNext = button(ui, "logs.searchNext");
  ui.replace({ text: "new loaded error" });
  assert.equal(input(ui).props.value, "");
  assert.equal(checkbox(ui).props.checked, false);
  assert.equal(marks(ui).length, 0);
  oldInput.props.onChange({ currentTarget: { value: "error" } });
  oldNext.props.onClick();
  await ui.flush();
  assert.equal(input(ui).props.value, "");
  assert.equal(content(pre(ui)), "new loaded error");
  ui.close();
});

test("language and region updates translate controls without altering loaded text or query", async () => {
  const text = "ERROR error",
    ui = fixture(text);
  await query(ui, "error");
  ui.replace({ language: "en-US", region: "en-US" });
  await ui.flush();
  assert.equal(input(ui).props.value, "error");
  assert.equal(content(pre(ui)), text);
  assert.equal(status(ui), "Match 1 of 2");
  assert.ok(
    input(ui).props.title.includes("Searches the log content currently loaded"),
  );
  ui.close();
});

test("without an owning main, navigation only moves the local pre viewport", async () => {
  const ui = fixture();
  await query(ui, "error");
  const first = marks(ui)[0];
  first.props.ref.current = {
    getBoundingClientRect: () => ({ top: 900, bottom: 920, height: 20 }),
  };
  const viewport = {
    getBoundingClientRect: () => ({ top: 100, bottom: 302 }),
    clientTop: 1,
    clientHeight: 200,
    scrollTop: 300,
  };
  pre(ui).props.ref.current = viewport;
  await click(ui, "logs.searchNext");
  assert.equal(viewport.scrollTop, 919);
  first.props.ref.current = {
    getBoundingClientRect: () => ({ top: 50, bottom: 70, height: 20 }),
  };
  await click(ui, "logs.searchPrevious");
  assert.equal(viewport.scrollTop, 868);
  ui.close();
});

function attachScrollScope(
  ui,
  { panelTop = 220, preTop = 300, markTop = 1000, clampMain = false } = {},
) {
  let mainScrollTop = 100;
  const main = {
    getBoundingClientRect: () => ({ top: 48, bottom: 500 }),
    clientTop: 0,
    clientHeight: 452,
    get scrollTop() {
      return mainScrollTop;
    },
    set scrollTop(value) {
      mainScrollTop = clampMain ? Math.min(100, value) : value;
    },
  };
  const mainOffset = () => main.scrollTop - 100;
  const viewport = {
    getBoundingClientRect: () => ({
      top: preTop - mainOffset(),
      bottom: preTop + 310 - mainOffset(),
    }),
    clientTop: 1,
    clientHeight: 308,
    scrollTop: 300,
  };
  const panel = {
    closest(selector) {
      assert.equal(selector, "main.content");
      return main;
    },
    getBoundingClientRect: () => ({
      top: panelTop - mainOffset(),
      bottom: panelTop + 390 - mainOffset(),
      height: 390,
    }),
  };
  const mark = {
    getBoundingClientRect() {
      const top = markTop - mainOffset() - (viewport.scrollTop - 300);
      return { top, bottom: top + 20, height: 20 };
    },
  };
  ui.tree.props.ref.current = panel;
  pre(ui).props.ref.current = viewport;
  marks(ui)[0].props.ref.current = mark;
  return { main, viewport, panel, mark };
}

test("navigation brings the fitted search panel into its main before positioning a bottom match", async () => {
  const ui = fixture();
  await query(ui, "error");
  const { main, viewport, panel, mark } = attachScrollScope(ui);
  await click(ui, "logs.searchNext");
  assert.equal(main.scrollTop, 218);
  assert.equal(viewport.scrollTop, 711);
  assert.ok(panel.getBoundingClientRect().top >= 56);
  assert.ok(panel.getBoundingClientRect().bottom <= 492);
  assert.ok(mark.getBoundingClientRect().bottom <= 492);
  ui.close();
});

test("navigation restores search controls above main and then reveals an earlier match", async () => {
  const ui = fixture();
  await query(ui, "error");
  const { main, viewport, panel, mark } = attachScrollScope(ui, {
    panelTop: 0,
    preTop: 80,
    markTop: 50,
  });
  await click(ui, "logs.searchPrevious");
  assert.equal(main.scrollTop, 44);
  assert.equal(viewport.scrollTop, 269);
  assert.equal(panel.getBoundingClientRect().top, 56);
  assert.equal(mark.getBoundingClientRect().top, 137);
  ui.close();
});

test("a clamped main uses its visible intersection with pre for match positioning", async () => {
  const ui = fixture();
  await query(ui, "error");
  const { main, viewport, mark } = attachScrollScope(ui, { clampMain: true });
  await click(ui, "logs.searchNext");
  assert.equal(main.scrollTop, 100);
  assert.equal(viewport.scrollTop, 828);
  assert.equal(mark.getBoundingClientRect().bottom, 492);
  ui.close();
});

test("cycling a single match reveals it again after the view was moved", async () => {
  const ui = fixture("only one result");
  await query(ui, "result");
  const { main, viewport } = attachScrollScope(ui);
  await click(ui, "logs.searchNext");
  assert.equal(active(ui), 0);
  assert.equal(main.scrollTop, 218);
  assert.equal(viewport.scrollTop, 711);
  ui.close();
});
