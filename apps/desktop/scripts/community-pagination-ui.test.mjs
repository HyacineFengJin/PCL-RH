/** Actual catalog event handlers with delayed, synthetic provider pages. */
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  communityHarness,
  elements,
  content,
  deferred,
  button,
} from "./community-test-harness.mjs";

const hit = (id = "Abcd1234", extra = {}) => ({
  project_id: id,
  title: `Project ${id}`,
  description: "Synthetic resource",
  source: "Modrinth",
  project_type: "mod",
  url: `https://modrinth.com/mod/${id}`,
  categories: ["fabric"],
  display_categories: ["utility"],
  versions: ["1.21.1"],
  ...extra,
});
const rows = (ui) =>
  elements(
    ui.tree,
    (node) => node.type === "article" && node.props.className === "ce-mod-row",
  );
const controls = (ui) =>
  elements(ui.tree, (node) => node.type?.name === "CeSelect");
const submit = (ui) =>
  elements(ui.tree, (node) => node.type === "form")[0].props.onSubmit({
    preventDefault() {},
  });
const change = (ui, index, value) =>
  controls(ui)[index].props.onChange({ target: { value } });
function fixture(extra = {}, options = {}) {
  const calls = [],
    requests = [],
    opened = [];
  const api = (command, args) => {
    calls.push({ command, args });
    const request = deferred();
    requests.push(request);
    return request.promise;
  };
  const ui = communityHarness(
    "CommunityCatalog",
    {
      api,
      scopeKey: "root-a",
      label: "模组",
      query: "",
      setQuery: (query) => ui.replace({ query }),
      onResourceDetails: (resource) => opened.push(resource),
      ...extra,
    },
    options,
  );
  const resolve = async (index, hits, more = false, extras = {}) => {
    requests[index].resolve({
      hits,
      warnings: [],
      pagination: { page: calls[index].args.page, has_next: more },
      ...extras,
    });
    await ui.flush();
  };
  return { ui, calls, requests, opened, resolve };
}
async function click(ui, key) {
  const target = button(ui, key);
  assert.equal(target.props.disabled, false);
  target.props.onClick();
  await ui.flush();
}

test("bottom controls use native page metadata and previous pages come from their raw cache", async () => {
  const f = fixture();
  assert.equal(f.calls[0].args.page, 1);
  assert.equal(button(f.ui, "community.previousPage").props.disabled, true);
  await f.resolve(0, [hit()], true);
  const next = button(f.ui, "community.nextPage").props.onClick;
  next();
  next();
  await f.ui.flush();
  assert.equal(f.calls.length, 2);
  assert.equal(f.calls[1].args.page, 2);
  assert.equal(button(f.ui, "community.nextPage").props.disabled, true);
  await f.resolve(1, [hit("Second12")], false);
  assert.ok(
    content(f.ui.tree).includes(
      f.ui.i18n.t("community.currentPage", { page: 2 }),
    ),
  );
  assert.equal(button(f.ui, "community.nextPage").props.disabled, true);
  await click(f.ui, "community.previousPage");
  assert.equal(f.calls.length, 2);
  assert.equal(
    rows(f.ui)[0].props["aria-label"],
    f.ui.i18n.t("ui.viewDetails", { name: "Project Abcd1234" }),
  );
  await click(f.ui, "community.nextPage");
  assert.equal(f.calls.length, 2);
  assert.equal(rows(f.ui).length, 1);
  f.ui.close();
});

test("verified aliases deduplicate later loaded pages without blocking progress on an empty visible page", async () => {
  const f = fixture();
  await f.resolve(
    0,
    [
      hit("Abcd1234", {
        sources: [
          {
            source: "CurseForge",
            project_id: "123",
            url: "https://www.curseforge.com/minecraft/mc-mods/example",
          },
        ],
      }),
    ],
    true,
  );
  await click(f.ui, "community.nextPage");
  await f.resolve(
    1,
    [
      hit("123", {
        source: "CurseForge",
        url: "https://www.curseforge.com/minecraft/mc-mods/example",
      }),
    ],
    true,
  );
  assert.equal(rows(f.ui).length, 0);
  assert.ok(
    content(f.ui.tree).includes(f.ui.i18n.t("community.pageDuplicates")),
  );
  await click(f.ui, "community.nextPage");
  assert.equal(f.calls[2].args.page, 3);
  await f.resolve(2, [hit("Unique34")], false);
  await click(f.ui, "community.previousPage");
  await click(f.ui, "community.previousPage");
  assert.equal(rows(f.ui).length, 1);
  assert.equal(f.calls.length, 3);
  f.ui.close();
});

test("same titles and slugs never deduplicate different projects or conflicting provider IDs", async () => {
  const f = fixture();
  await f.resolve(
    0,
    [
      hit("Abcd1234", {
        title: "Same name",
        url: "https://modrinth.com/mod/shared",
      }),
    ],
    true,
  );
  await click(f.ui, "community.nextPage");
  await f.resolve(
    1,
    [
      hit("Different", {
        title: "Same name",
        url: "https://modrinth.com/mod/shared",
      }),
      hit("123", {
        source: "CurseForge",
        title: "Same name",
        url: "https://www.curseforge.com/minecraft/mc-mods/shared",
      }),
    ],
    false,
  );
  assert.equal(rows(f.ui).length, 2);
  f.ui.close();
});

test("a broad encyclopedia entry cannot hide multiple provider forks across loaded pages", async () => {
  const f = fixture();
  await f.resolve(
    0,
    [
      hit("123", {
        source: "MC百科",
        url: "https://www.mcmod.cn/class/123.html",
        sources: [
          {
            source: "Modrinth",
            project_id: null,
            url: "https://modrinth.com/mod/fork-one",
          },
          {
            source: "Modrinth",
            project_id: null,
            url: "https://modrinth.com/mod/fork-two",
          },
        ],
      }),
    ],
    true,
  );
  await click(f.ui, "community.nextPage");
  await f.resolve(
    1,
    [
      hit("Fork1111", { url: "https://modrinth.com/mod/fork-one" }),
      hit("Fork2222", { url: "https://modrinth.com/mod/fork-two" }),
    ],
    false,
  );
  assert.equal(rows(f.ui).length, 2);
  f.ui.close();
});

test("a partial provider warning coexists with successful cards and their real continuation", async () => {
  const f = fixture();
  await f.resolve(0, [hit()], true, {
    warnings: ["CurseForge: synthetic failure"],
  });
  assert.equal(rows(f.ui).length, 1);
  assert.ok(content(f.ui.tree).includes("CurseForge: synthetic failure"));
  assert.equal(button(f.ui, "community.nextPage").props.disabled, false);
  f.ui.close();
});

test("visible card count never invents a continuation when metadata says no next page", async () => {
  const f = fixture();
  await f.resolve(
    0,
    Array.from({ length: 30 }, (_, index) => hit(`Proj${index}`)),
    false,
  );
  assert.equal(button(f.ui, "community.nextPage").props.disabled, true);
  f.ui.close();
});

test("an error on page two retries that page and retains access to page one", async () => {
  const f = fixture();
  await f.resolve(0, [hit()], true);
  await click(f.ui, "community.nextPage");
  f.requests[1].reject(Error("Synthetic second page failure"));
  await f.ui.flush();
  assert.ok(content(f.ui.tree).includes("Synthetic second page failure"));
  assert.equal(button(f.ui, "community.previousPage").props.disabled, false);
  button(f.ui, "ui.retry").props.onClick();
  await f.ui.flush();
  assert.equal(f.calls[2].args.page, 2);
  await f.resolve(2, [hit("Retry123")], false);
  assert.equal(rows(f.ui).length, 1);
  await click(f.ui, "community.previousPage");
  assert.equal(f.calls.length, 3);
  assert.equal(rows(f.ui).length, 1);
  f.ui.close();
});

test("response metadata for another page is rejected instead of displaying its cards", async () => {
  const f = fixture();
  await f.resolve(0, [hit()], true, {
    pagination: { page: 2, has_next: true },
  });
  assert.equal(rows(f.ui).length, 0);
  assert.ok(content(f.ui.tree).includes(f.ui.i18n.t("community.invalidPage")));
  f.ui.close();
});

for (const [index, value, argument, expected] of [
  [0, "CurseForge", "source", "CurseForge"],
  [1, "utility", "tag", "utility"],
  [2, "更新时间", "sort", "updated"],
  [3, "1.21.1", "version", "1.21.1"],
  [4, "Fabric", "loader", "fabric"],
])
  test(`changing ${argument} immediately retires the previous page and starts page one`, async () => {
    const f = fixture();
    await f.resolve(0, [hit()], true);
    await click(f.ui, "community.nextPage");
    change(f.ui, index, value);
    await f.ui.flush();
    assert.equal(f.calls.at(-1).args.page, 1);
    assert.equal(f.calls.at(-1).args[argument], expected);
    const latest = f.requests.length - 1;
    await f.resolve(latest, [hit("Filtered")], false);
    f.requests[1].resolve({
      hits: [hit("OldPage2")],
      warnings: ["old warning"],
      pagination: { page: 2, has_next: true },
    });
    await f.ui.flush();
    assert.ok(content(f.ui.tree).includes("Project Filtered"));
    assert.ok(!content(f.ui.tree).includes("OldPage2"));
    assert.ok(!content(f.ui.tree).includes("old warning"));
    f.ui.close();
  });

test("draft search text resets to page one, rejects late errors and fetches only after submit", async () => {
  const f = fixture();
  await f.resolve(0, [hit()], true);
  await click(f.ui, "community.nextPage");
  const oldSubmit = elements(f.ui.tree, (node) => node.type === "form")[0].props
    .onSubmit;
  elements(f.ui.tree, (node) => node.type === "input")[0].props.onChange({
    target: { value: "new query" },
  });
  await f.ui.flush();
  assert.equal(f.calls.length, 2);
  assert.ok(
    content(f.ui.tree).includes(
      f.ui.i18n.t("community.currentPage", { page: 1 }),
    ),
  );
  f.requests[1].reject(Error("old query error"));
  await f.ui.flush();
  oldSubmit({ preventDefault() {} });
  await f.ui.flush();
  assert.equal(f.calls.length, 2);
  assert.ok(!content(f.ui.tree).includes("old query error"));
  assert.equal(button(f.ui, "community.nextPage").props.disabled, true);
  submit(f.ui);
  await f.ui.flush();
  assert.equal(f.calls[2].args.query, "new query");
  assert.equal(f.calls[2].args.page, 1);
  await f.resolve(2, [hit("NewQuery")], false);
  f.ui.close();
});

test("root A → B → A retires old page, filter and row handlers and restarts page one", async () => {
  const f = fixture();
  await f.resolve(0, [hit()], true);
  const oldNext = button(f.ui, "community.nextPage").props.onClick,
    oldFilter = controls(f.ui)[0].props.onChange,
    oldRow = rows(f.ui)[0].props.onClick;
  f.ui.replace({ scopeKey: "root-b" });
  await f.ui.flush();
  f.ui.replace({ scopeKey: "root-a" });
  await f.ui.flush();
  assert.equal(f.calls.length, 3);
  oldNext();
  oldFilter({ target: { value: "CurseForge" } });
  oldRow();
  await f.ui.flush();
  assert.equal(f.calls.length, 3);
  assert.equal(f.opened.length, 0);
  assert.equal(f.calls.at(-1).args.page, 1);
  f.requests[1].resolve({
    hits: [hit("RetiredB")],
    warnings: [],
    pagination: { page: 1, has_next: false },
  });
  await f.resolve(2, [hit("CurrentA")], false);
  assert.ok(!content(f.ui.tree).includes("RetiredB"));
  f.ui.close();
});

test("an API replacement on page two clears loaded pages and rejects its late response", async () => {
  const f = fixture();
  await f.resolve(0, [hit()], true);
  await click(f.ui, "community.nextPage");
  const replacement = deferred(),
    calls = [];
  f.ui.replace({
    api: (command, args) => {
      calls.push({ command, args });
      return replacement.promise;
    },
  });
  await f.ui.flush();
  assert.equal(calls[0].args.page, 1);
  f.requests[1].resolve({
    hits: [hit("Retired2")],
    warnings: [],
    pagination: { page: 2, has_next: true },
  });
  replacement.resolve({
    hits: [hit("Replaced")],
    warnings: [],
    pagination: { page: 1, has_next: false },
  });
  await f.ui.flush();
  assert.ok(!content(f.ui.tree).includes("Retired2"));
  assert.ok(content(f.ui.tree).includes("Replaced"));
  f.ui.close();
});

test("MC encyclopedia uses provider continuation and selecting it clears unsupported filters", async () => {
  const f = fixture({ compatibility: { version: "1.21.1", loader: "fabric" } });
  change(f.ui, 0, "MC百科");
  await f.ui.flush();
  const latest = f.calls.length - 1;
  assert.equal(f.calls[latest].args.page, 1);
  for (const key of ["version", "loader", "tag", "sort"])
    assert.equal(f.calls[latest].args[key], undefined);
  await f.resolve(
    latest,
    [
      hit("123", {
        source: "MC百科",
        url: "https://www.mcmod.cn/class/123.html",
      }),
    ],
    true,
  );
  await click(f.ui, "community.nextPage");
  assert.equal(f.calls.at(-1).args.page, 2);
  assert.equal(f.calls.at(-1).args.source, "MC百科");
  await f.resolve(f.calls.length - 1, [], false);
  assert.equal(button(f.ui, "community.nextPage").props.disabled, true);
  f.ui.close();
});

test("each supported resource kind requests its actual provider page while worlds stay unavailable", async () => {
  for (const [label, projectType] of [
    ["模组", "mod"],
    ["整合包", "modpack"],
    ["数据包", "datapack"],
    ["资源包", "resourcepack"],
    ["光影包", "shader"],
  ]) {
    const f = fixture({ label });
    await f.resolve(0, [hit()], true);
    await click(f.ui, "community.nextPage");
    assert.equal(f.calls[1].args.projectType, projectType);
    assert.equal(f.calls[1].args.page, 2);
    f.ui.close();
  }
  const f = fixture({ label: "世界" });
  assert.equal(f.calls.length, 0);
  assert.ok(
    content(f.ui.tree).includes(f.ui.i18n.t("download.worldsUnavailable")),
  );
  assert.equal(elements(f.ui.tree, (node) => node.type === "nav").length, 0);
  f.ui.close();
});

test("closing the catalog retires late pages and retained pagination handlers", async () => {
  const f = fixture();
  await f.resolve(0, [hit()], true);
  const oldNext = button(f.ui, "community.nextPage").props.onClick;
  await click(f.ui, "community.nextPage");
  f.ui.close();
  f.requests[1].resolve({
    hits: [hit("Closed")],
    warnings: [],
    pagination: { page: 2, has_next: true },
  });
  await f.ui.flush();
  oldNext();
  await f.ui.flush();
  assert.equal(f.calls.length, 2);
});

test("loaded page storage is bounded and an older page is never filtered by a newer page", () => {
  const f = fixture();
  const { CommunityPages } = f.ui.module("communityPagination");
  const cache = new CommunityPages();
  for (let page = 1; page <= 9; page++)
    cache.remember(page, {
      hits: [hit()],
      warnings: [],
      pagination: { page, has_next: true },
    });
  assert.equal(cache.get(1), undefined);
  assert.equal(cache.visibleHits(2).length, 1);
  assert.equal(cache.visibleHits(9).length, 0);
  f.ui.close();
});

test("native continuation permits page 101 independently of the eight-page cache", async () => {
  const f = fixture();
  for (let page = 1; page <= 100; page++) {
    await f.resolve(page - 1, [hit(`Page${page}`)], true);
    await click(f.ui, "community.nextPage");
  }
  assert.equal(f.calls.at(-1).args.page, 101);
  await f.resolve(100, [hit("Page101")], false);
  assert.ok(
    content(f.ui.tree).includes(
      f.ui.i18n.t("community.currentPage", { page: 101 }),
    ),
  );
  f.ui.close();
});

function scrollFixture() {
  const scrolls = [];
  let focusChanges = 0;
  const main = {
    scrollTop: 1200,
    clientTop: 2,
    getBoundingClientRect: () => ({ top: 100 }),
    scrollTo(options) {
      scrolls.push(options);
      main.scrollTop = options.top;
    },
    focus() {
      focusChanges++;
    },
  };
  const section = {
    closest(selector) {
      assert.equal(selector, "main.content");
      return main;
    },
    getBoundingClientRect: () => ({ top: 100 + 500 - main.scrollTop }),
    focus() {
      focusChanges++;
    },
    scrollIntoView() {
      throw Error("Must not scroll global ancestors");
    },
  };
  const f = fixture(
    {},
    {
      mountRefs(tree) {
        const target = elements(
          tree,
          (node) => node.type === "section" && node.props.ref,
        )[0];
        assert.ok(target, "Results section owns a DOM ref");
        target.props.ref.current = section;
      },
    },
  );
  return { ...f, main, scrolls, focusChanges: () => focusChanges };
}

test("explicit pagination scrolls the main viewport once after success and once on cached return", async () => {
  const f = scrollFixture();
  await f.resolve(0, [hit()], true);
  assert.deepEqual(f.scrolls, []);
  await click(f.ui, "community.nextPage");
  assert.deepEqual(f.scrolls, []);
  await f.resolve(1, [hit("Second12")], false);
  assert.deepEqual(f.scrolls, [{ top: 490, behavior: "auto" }]);
  assert.equal(f.focusChanges(), 0);
  await f.ui.flush();
  assert.equal(f.scrolls.length, 1);
  f.main.scrollTop = 1500;
  await click(f.ui, "community.previousPage");
  assert.deepEqual(f.scrolls, [
    { top: 490, behavior: "auto" },
    { top: 490, behavior: "auto" },
  ]);
  assert.equal(f.calls.length, 2);
  assert.equal(f.focusChanges(), 0);
  f.ui.close();
});

test("a late page from a retired query cannot scroll a replacement query", async () => {
  const f = scrollFixture();
  await f.resolve(0, [hit()], true);
  await click(f.ui, "community.nextPage");
  change(f.ui, 0, "CurseForge");
  await f.ui.flush();
  assert.equal(f.calls[2].args.page, 1);
  await f.resolve(2, [hit("Current1")], false);
  await f.resolve(1, [hit("Retired2")], false);
  assert.deepEqual(f.scrolls, []);
  assert.equal(f.focusChanges(), 0);
  f.ui.close();
});

test("failed pagination does not scroll and a successful deliberate retry consumes the intent once", async () => {
  const f = scrollFixture();
  await f.resolve(0, [hit()], true);
  await click(f.ui, "community.nextPage");
  f.requests[1].reject(Error("Synthetic page failure"));
  await f.ui.flush();
  assert.deepEqual(f.scrolls, []);
  button(f.ui, "ui.retry").props.onClick();
  await f.ui.flush();
  await f.resolve(2, [hit("Retried2")], false);
  assert.equal(f.scrolls.length, 1);
  await f.ui.flush();
  assert.equal(f.scrolls.length, 1);
  assert.equal(f.focusChanges(), 0);
  f.ui.close();
});
