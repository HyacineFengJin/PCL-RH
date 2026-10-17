/** Directory regressions execute the real preview API, menu/dialog JSX and
 * request handlers with synthetic state; no native app or user files are read. */
import assert from "node:assert/strict";
import fs from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";
const require = createRequire(import.meta.url);
const ts = require("typescript");
const source = fs.readFileSync(
  new URL("../src/main.tsx", import.meta.url),
  "utf8",
);
const tree = ts.createSourceFile(
  "main.tsx",
  source,
  ts.ScriptTarget.Latest,
  true,
  ts.ScriptKind.TSX,
);

function find(predicate) {
  let found;
  function visit(node) {
    if (!found && predicate(node)) found = node;
    if (!found) ts.forEachChild(node, visit);
  }
  visit(tree);
  assert.ok(found, "Expected production source node");
  return found.getText(tree);
}
function declaration(name) {
  return find(
    (node) => ts.isFunctionDeclaration(node) && node.name?.text === name,
  );
}
function evaluate(code, environment = {}) {
  const compiled = ts.transpileModule(code, {
    compilerOptions: {
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.CommonJS,
      jsx: ts.JsxEmit.ReactJSX,
    },
  }).outputText;
  const module = { exports: {} };
  new Function(
    "environment",
    "require",
    "module",
    "exports",
    `let {${Object.keys(environment).join(",")}} = environment;\n${compiled}`,
  )(environment, require, module, module.exports);
  return module.exports.value;
}
function functions(names, environment) {
  return evaluate(
    `${names.map(declaration).join("\n")}\nmodule.exports.value = {${names.join(",")}};`,
    environment,
  );
}
function jsx(className, environment, includes = "") {
  const markup = find(
    (node) =>
      ts.isJsxElement(node) &&
      node.openingElement.attributes.properties.some(
        (attribute) =>
          ts.isJsxAttribute(attribute) &&
          attribute.name.text === "className" &&
          ts.isStringLiteral(attribute.initializer) &&
          attribute.initializer.text === className,
      ) &&
      node.getText(tree).includes(includes),
  );
  return evaluate(`module.exports.value = (${markup});`, environment);
}
function elements(node, predicate) {
  if (Array.isArray(node))
    return node.flatMap((child) => elements(child, predicate));
  if (!node || typeof node !== "object") return [];
  return [
    ...(predicate(node) ? [node] : []),
    ...elements(node.props?.children, predicate),
  ];
}
function text(node) {
  if (Array.isArray(node)) return node.map(text).join("");
  return node && typeof node === "object"
    ? text(node.props?.children)
    : (node ?? "");
}
function translator(language) {
  const code = fs.readFileSync(
    new URL(`../src/locales/app${language}.ts`, import.meta.url),
    "utf8",
  );
  const app = evaluate(`${code}\nmodule.exports.value = messages;`);
  const localeSource = fs.readFileSync(
    new URL(
      `../src/locales/${language === "Zh" ? "zh-CN" : "en-US"}.ts`,
      import.meta.url,
    ),
    "utf8",
  );
  const locale = ts.createSourceFile(
    "locale.ts",
    localeSource,
    ts.ScriptTarget.Latest,
    true,
  );
  const messages = locale.statements
    .filter(ts.isVariableStatement)
    .flatMap((statement) => statement.declarationList.declarations)
    .find((item) => item.name.getText(locale) === "messages").initializer;
  const catalog = evaluate(
    `module.exports.value = ${messages.getText(locale)};`,
    {
      app,
      experimental: {},
      pages: {},
    },
  );
  return (key, params = {}) => {
    assert.equal(
      typeof catalog[key],
      "string",
      `Missing ${language} message ${key}`,
    );
    return catalog[key].replace(
      /\{(\w+)\}/g,
      (token, name) => params[name] ?? token,
    );
  };
}
function root(id, available = true) {
  return {
    id,
    name: id,
    path: `/synthetic/${id}`,
    available,
    selected: `${id}-instance`,
  };
}
function state(roots, active) {
  const settings = {
    root_id: active.id,
    root: active.path,
    selected: active.selected,
    player: "Fixture",
    memory_gib: 6,
  };
  return {
    roots,
    settings,
    instances: [{ id: active.selected }],
    status: { stage: "idle" },
    auth: {},
  };
}
function previewApi(roots, active, extra = {}) {
  const preview = { ...state(roots, active), ...extra };
  const previewRoots = new Map(
    roots.map((item) => [
      item.id,
      {
        settings: {
          ...preview.settings,
          root_id: item.id,
          root: item.path,
          selected: item.selected,
        },
        instances: [{ id: item.selected }],
        resources: [],
        servers: [],
        scan_issues: [],
      },
    ]),
  );
  return functions(["normalizedState", "selectPreviewRoot", "api"], {
    preview,
    previewRoots,
    native: false,
    emptyAuth: {},
    preparePreviewRoots() {},
  }).api;
}

test("preview removal chooses the next usable root, wraps, and retains an offline neighbour", async () => {
  for (const fixture of [
    {
      roots: [
        root("before"),
        root("removed"),
        root("offline", false),
        root("after"),
      ],
      removed: 1,
      next: "after",
    },
    {
      roots: [root("before"), root("removed"), root("offline", false)],
      removed: 1,
      next: "before",
    },
    {
      roots: [root("first"), root("neighbour"), root("removed")],
      removed: 2,
      next: "neighbour",
    },
    {
      roots: [root("before", false), root("removed"), root("after", false)],
      removed: 1,
      next: "after",
    },
    {
      roots: [root("before", false), root("neighbour", false), root("removed")],
      removed: 2,
      next: "neighbour",
    },
  ]) {
    const removed = fixture.roots[fixture.removed];
    const api = previewApi(fixture.roots, removed);
    const result = await api("root_remove", { id: removed.id });
    assert.equal(result.settings.root_id, fixture.next);
    assert.deepEqual(
      result.roots.map((item) => item.id),
      fixture.roots.filter((item) => item !== removed).map((item) => item.id),
    );
    const next = result.roots.find((item) => item.id === fixture.next);
    assert.deepEqual(
      result.instances.map((item) => item.id),
      next.available ? [next.selected] : [],
    );
    await assert.rejects(api("root_select", { id: removed.id }), /不存在/);
  }
});

test("preview removal preserves the active selection and rejects last or occupied roots", async () => {
  const active = root("active"),
    missing = root("missing", false);
  const unchanged = await previewApi([active, missing], active)("root_remove", {
    id: missing.id,
  });
  assert.equal(unchanged.settings.root_id, active.id);
  assert.deepEqual(unchanged.instances, [{ id: active.selected }]);
  await assert.rejects(
    previewApi([active], active)("root_remove", { id: active.id }),
    /至少保留/,
  );
  for (const extra of [
    { status: { stage: "running", root_id: missing.id } },
    { download_status: { stage: "processing", root_id: missing.id } },
  ])
    await assert.rejects(
      previewApi(
        [active, missing],
        active,
        extra,
      )("root_remove", { id: missing.id }),
      /正在使用/,
    );
});

for (const language of ["Zh", "En"]) {
  test(`${language} directory menu explains the last root and occupied root`, () => {
    const t = translator(language),
      item = root("Fixture", false),
      actions = [];
    const environment = {
      data: { roots: [item] },
      menuRoot: item,
      rootMenu: { x: 0, y: 0 },
      menuRootIndex: 0,
      rootWorking: false,
      menuRootOccupied: false,
      t,
      editRoot: (...args) => actions.push(args),
      rootAction() {},
      Pencil() {},
      ArrowUp() {},
      ArrowDown() {},
      X() {},
    };
    const remove = (extra) =>
      elements(
        jsx("ce-root-menu", { ...environment, ...extra }),
        (node) => node.type === "button",
      ).at(-1);
    assert.equal(remove().props.disabled, true);
    assert.equal(remove().props.title, t("folders.lastRoot"));
    assert.equal(
      remove({ menuRootOccupied: true }).props.title,
      t("folders.occupied"),
    );
    const enabled = remove({ data: { roots: [item, root("Other")] } });
    assert.equal(enabled.props.disabled, false);
    enabled.props.onClick();
    assert.deepEqual(actions, [[item, "remove"]]);
  });

  test(`${language} unavailable hint and confirmation describe removing an entry`, () => {
    const t = translator(language),
      item = root("Fixture", false),
      calls = [];
    const notice = (count) =>
      jsx(
        "error-banner",
        {
          selectedRoot: item,
          data: {
            roots: Array.from({ length: count }, () => item),
            scan_error: "Unavailable",
          },
          t,
          TriangleAlert() {},
        },
        "folders.unavailableLastHelp",
      );
    assert.ok(text(notice(1)).includes(t("folders.unavailableLastHelp")));
    assert.ok(text(notice(2)).includes(t("folders.unavailableHelp")));
    for (const available of [false, true]) {
      const dialog = jsx("rd-name-dialog ce-root-dialog", {
        rootDialog: { root: { ...item, available }, mode: "remove" },
        rootWorking: false,
        rootError: "",
        t,
        rootAction: (...args) => calls.push(args),
        setRootDialog() {},
      });
      assert.ok(
        text(dialog).includes(
          t(
            available ? "folders.removeHelp" : "folders.removeUnavailableHelp",
            { name: item.name },
          ),
        ),
      );
      dialog.props.onSubmit({ preventDefault() {} });
    }
    assert.deepEqual(calls, [
      ["root_remove", { id: item.id }],
      ["root_remove", { id: item.id }],
    ]);
  });
}

test("a delayed bootstrap cannot resurrect a removed directory or its old instance", async () => {
  let resolveOld, current;
  const old = root("old"),
    next = root("next"),
    pending = new Promise((resolve) => {
      resolveOld = resolve;
    });
  const environment = {
    rootWorkingRef: { current: false },
    terminalRefresh: { current: false },
    terminalRead: { current: null },
    stateRequest: { current: 0 },
    rootDialog: null,
    viewContext: {
      current: { rootKey: `${old.id}:${old.path}`, selected: old.selected },
    },
    emptyAuth: {},
    api: (command) =>
      command === "bootstrap" ? pending : Promise.resolve(state([next], next)),
    setData: (value) => {
      current = value;
    },
    setDraft() {},
    setError() {},
    setScreen() {},
    setTaskOrigin() {},
    setRootWorking() {},
    setRootError() {},
    setRootMenu() {},
    setRootDialog() {},
    notify() {},
  };
  const handlers = functions(
    [
      "normalizedState",
      "applyState",
      "load",
      "rootAction",
      "flushTerminalRefresh",
    ],
    environment,
  );
  const loading = handlers.load();
  await handlers.rootAction("root_remove", { id: old.id });
  resolveOld(state([old, next], old));
  await loading;
  assert.equal(current.settings.root_id, next.id);
  assert.deepEqual(
    current.roots.map((item) => item.id),
    [next.id],
  );
  assert.deepEqual(
    current.instances.map((item) => item.id),
    [next.selected],
  );
});

function saveFixture(reply) {
  const active = root("save-root");
  const data = state([active], active);
  data.settings = {
    ...data.settings,
    revision: "original-revision",
    isolation: "automatic",
    isolation_overrides: {},
    window_title_overrides: {},
  };
  let current = structuredClone(data);
  let draft = structuredClone(data.settings);
  const contextKey = { current: `${active.id}:${active.path}` };
  const loads = [];
  const calls = [];
  const { save } = functions(["save"], {
    data,
    rootId: active.id,
    contextKey,
    api: async (command, request) => {
      calls.push({ command, request });
      assert.equal(command, "save_settings");
      return reply
        ? reply(request.settings)
        : { ...request.settings, revision: "saved-revision" };
    },
    setData: (update) => {
      current = update(current);
    },
    setDraft: (update) => {
      draft = update(draft);
    },
    load: async () => loads.push(contextKey.current),
  });
  return {
    save,
    settings: data.settings,
    loads,
    calls,
    current: () => current,
    draft: () => draft,
    switchRoot(next) {
      current = state([active, next], next);
      current.settings.revision = "next-root-revision";
      draft = structuredClone(current.settings);
      contextKey.current = `${next.id}:${next.path}`;
    },
  };
}

test("successful global and instance isolation saves reload the root scan", async () => {
  for (const change of [
    { isolation: "isolated" },
    { isolation_overrides: { Example: "shared" } },
  ]) {
    const fixture = saveFixture();
    await fixture.save({ ...fixture.settings, ...change });
    assert.deepEqual(fixture.loads, ["save-root:/synthetic/save-root"]);
    assert.equal(fixture.current().settings.revision, "saved-revision");
    assert.equal(fixture.draft().revision, "saved-revision");
    for (const [key, value] of Object.entries(change))
      assert.deepEqual(fixture.current().settings[key], value);
  }
});

test("title and memory saves keep their ordinary path without reloading the scan", async () => {
  for (const change of [
    { window_title_overrides: { Example: "New title" } },
    { memory_gib: 10 },
  ]) {
    const fixture = saveFixture();
    await fixture.save({ ...fixture.settings, ...change });
    assert.deepEqual(fixture.loads, []);
    assert.equal(fixture.current().settings.revision, "saved-revision");
    for (const [key, value] of Object.entries(change))
      assert.deepEqual(fixture.current().settings[key], value);
  }
});

test("a delayed isolation save cannot reload or replace a newly selected root", async () => {
  let resolveSave;
  const pending = new Promise((resolve) => {
    resolveSave = resolve;
  });
  const fixture = saveFixture(() => pending);
  const submitted = { ...fixture.settings, isolation: "isolated" };
  const saving = fixture.save(submitted);
  fixture.switchRoot(root("next-save-root"));
  const selected = structuredClone(fixture.current());
  const draft = structuredClone(fixture.draft());
  resolveSave({ ...submitted, revision: "late-old-root-revision" });
  await saving;
  assert.deepEqual(fixture.current(), selected);
  assert.deepEqual(fixture.draft(), draft);
  assert.deepEqual(fixture.loads, []);
});

test("failed isolation saves leave the current scan and settings unchanged", async () => {
  for (const change of [
    { isolation: "shared" },
    { isolation_overrides: { Example: "isolated" } },
  ]) {
    const fixture = saveFixture(async () => {
      throw new Error("synthetic rejected save");
    });
    const original = structuredClone(fixture.current());
    const draft = structuredClone(fixture.draft());
    await assert.rejects(
      fixture.save({ ...fixture.settings, ...change }),
      /synthetic rejected save/,
    );
    assert.deepEqual(fixture.current(), original);
    assert.deepEqual(fixture.draft(), draft);
    assert.deepEqual(fixture.loads, []);
  }
});
