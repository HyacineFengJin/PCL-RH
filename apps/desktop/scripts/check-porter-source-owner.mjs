/** Exact volatile-token ownership only; no desktop, host or provider runs. */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
const ts = createRequire(import.meta.url)("typescript");
const compiled = ts.transpileModule(
  readFileSync(new URL("../src/porterSourceOwner.ts", import.meta.url), "utf8"),
  {
    compilerOptions: {
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.CommonJS,
    },
  },
).outputText;
const module = { exports: {} };
new Function("exports", compiled)(module.exports);
const { createPorterSourceOwner } = module.exports;
const one = "00000000-0000-4000-8000-000000000001",
  two = "00000000-0000-4000-8000-000000000002";
const flush = () => new Promise(setImmediate);

// Execute the actual production import callback and its action fence, with
// deterministic transport replies. Other page rendering is irrelevant here.
const workspaceSource = readFileSync(
  new URL("../src/ExperimentalPorterWorkspace.tsx", import.meta.url),
  "utf8",
);
const ast = ts.createSourceFile(
  "workspace.tsx",
  workspaceSource,
  ts.ScriptTarget.Latest,
  true,
  ts.ScriptKind.TSX,
);
const workspace = ast.statements.find(
  (n) =>
    ts.isFunctionDeclaration(n) &&
    n.name?.text === "ExperimentalPorterWorkspace",
);
const perform = workspace.body.statements.find(
  (n) => ts.isFunctionDeclaration(n) && n.name?.text === "perform",
);
let importCallback;
function visit(node) {
  if (ts.isJsxAttribute(node) && node.name.getText(ast) === "onImport")
    importCallback = node.initializer.expression.getText(ast);
  ts.forEachChild(node, visit);
}
visit(workspace);
assert.ok(perform && importCallback);
const handlers = ts.transpileModule(
  perform.getText(ast) + "\nconst onImport = " + importCallback + ";",
  {
    compilerOptions: {
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.CommonJS,
    },
  },
).outputText;
function importFixture() {
  let valid = true,
    resolveImport;
  const released = [],
    accepted = [],
    sourceOwner = createPorterSourceOwner();
  const releaseSource = async (id) => released.push(id);
  sourceOwner.adopt(one, releaseSource);
  const context = {
    interactive: () => valid,
    current: () => valid,
    native: true,
    scope: {},
    working: { current: null },
    setBusyScope() {},
    setError() {},
    projectId: "",
    sourceOwner,
    releaseSource,
    refreshList: async () => {},
    serviceError: String,
    setSource: (value) => accepted.push(value),
    setDirectory() {},
    setAllowed() {},
    setIdentifierDeclared() {},
    api: async () => ({ status: "selected", path: "synthetic-source" }),
    call: async () =>
      new Promise((resolve) => {
        resolveImport = resolve;
      }),
  };
  const onImport = new Function(
    ...Object.keys(context),
    handlers + "\nreturn onImport;",
  )(...Object.values(context));
  return {
    released,
    accepted,
    sourceOwner,
    onImport,
    retire: () => {
      valid = false;
    },
    reply: () =>
      resolveImport({
        sourceId: two,
        files: { "Fixture.java": "synthetic" },
        skipped: [],
      }),
  };
}

test("replacement, clear and final owner disposal release exact tokens through their creator", async () => {
  const calls = [],
    owner = createPorterSourceOwner();
  const first = async (id) => calls.push(["first-transport", id]);
  const second = async (id) => calls.push(["second-transport", id]);
  assert.equal(owner.adopt(one, first), true);
  // A page leaving and returning keeps this same parent-owned draft.
  assert.deepEqual(calls, []);
  assert.equal(owner.adopt(one, second), true);
  await flush();
  assert.deepEqual(calls, []);
  assert.equal(owner.adopt(two, second), true);
  await flush();
  assert.deepEqual(calls, [["first-transport", one]]);
  await owner.clear();
  assert.deepEqual(calls, [
    ["first-transport", one],
    ["second-transport", two],
  ]);
  await owner.clear();
  await owner.dispose();
  assert.equal(calls.length, 2);
  assert.equal(owner.adopt(one, first), false);
  await flush();
  assert.deepEqual(calls.at(-1), ["first-transport", one]);
});

test("a late unaccepted import is released without replacing the live draft", async () => {
  const calls = [],
    owner = createPorterSourceOwner(),
    release = async (id) => calls.push(id);
  owner.adopt(one, release);
  await owner.discard(two, release);
  assert.deepEqual(calls, [two]);
  await owner.dispose();
  assert.deepEqual(calls, [two, one]);
  assert.throws(() => owner.adopt("arbitrary path", release), /Invalid/);
  await owner.discard("arbitrary path", release);
  assert.equal(calls.length, 2);
});

test("cleanup transport failure is observed and does not issue another operation", async () => {
  const owner = createPorterSourceOwner();
  let calls = 0;
  owner.adopt(one, async (id) => {
    calls++;
    assert.equal(id, one);
    throw new Error("retired transport");
  });
  await owner.dispose();
  assert.equal(calls, 1);
});

test("actual import callback releases the replaced draft and retains the accepted receipt", async () => {
  const fixture = importFixture();
  fixture.onImport();
  await flush();
  fixture.reply();
  await flush();
  assert.deepEqual(fixture.released, [one]);
  assert.equal(fixture.accepted[0].sourceId, two);
  await fixture.sourceOwner.dispose();
  assert.deepEqual(fixture.released, [one, two]);
});

test("actual import callback releases a late token after the page scope retires", async () => {
  const fixture = importFixture();
  fixture.onImport();
  await flush();
  fixture.retire();
  fixture.reply();
  await flush();
  assert.deepEqual(fixture.released, [two]);
  assert.equal(fixture.accepted.length, 0);
  await fixture.sourceOwner.dispose();
  assert.deepEqual(fixture.released, [two, one]);
});

test("actual parent draft hook retains receipts across effect replay and releases only on final disposal", async () => {
  const hookSource = ts.transpileModule(
    readFileSync(
      new URL("../src/usePorterDraftOwner.ts", import.meta.url),
      "utf8",
    ),
    {
      compilerOptions: {
        target: ts.ScriptTarget.ES2022,
        module: ts.ModuleKind.CommonJS,
      },
    },
  ).outputText;
  const refs = [];
  let cursor = 0,
    effect;
  const react = {
    useRef(value) {
      return (refs[cursor++] ??= { current: value });
    },
    useEffect(value) {
      effect = value;
    },
  };
  const exports = {};
  new Function("require", "exports", hookSource)(
    (name) => (name === "react" ? react : module.exports),
    exports,
  );
  const render = () => {
    cursor = 0;
    return exports.usePorterDraftOwner();
  };
  const draft = render(),
    released = [];
  const cleanup = effect();
  draft.sourceOwner.adopt(one, async (id) => released.push(id));
  cleanup();
  const replay = render(),
    finalCleanup = effect();
  assert.equal(replay.drafts, draft.drafts);
  assert.equal(replay.sourceOwner, draft.sourceOwner);
  await flush();
  assert.deepEqual(released, []);
  finalCleanup();
  await flush();
  assert.deepEqual(released, [one]);
});
