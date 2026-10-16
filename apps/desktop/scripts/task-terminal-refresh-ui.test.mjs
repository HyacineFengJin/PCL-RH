/** Actual App handlers with synthetic root/task replies; no native or user data. */
import assert from "node:assert/strict";
import fs from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";

const require = createRequire(import.meta.url),
  ts = require("typescript"),
  source = fs.readFileSync(new URL("../src/main.tsx", import.meta.url), "utf8"),
  ast = ts.createSourceFile(
    "main.tsx",
    source,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX,
  );
const lifecycle = { exports: {} };
new Function(
  "module",
  "exports",
  ts.transpileModule(
    fs.readFileSync(
      new URL("../src/taskLifecycle.ts", import.meta.url),
      "utf8",
    ),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText,
)(lifecycle, lifecycle.exports);

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
async function flush() {
  await new Promise((resolve) => setImmediate(resolve));
}
function snapshot(root = "root-a", extra = {}) {
  return { settings: { root_id: root, root: `/synthetic/${root}` }, ...extra };
}
function task(extra = {}) {
  return {
    task_id: "install-a",
    kind: "install",
    stage: "complete",
    root_id: "root-a",
    version: "synthetic-install",
    message: "Synthetic task",
    completed: 0,
    total: 0,
    bytes_done: 0,
    bytes_total: 0,
    ...extra,
  };
}

function appHandlers(reply = async () => snapshot()) {
  const app = ast.statements.find(
    (node) => ts.isFunctionDeclaration(node) && node.name?.text === "App",
  );
  assert.ok(app?.body);
  const names = new Set([
    "handleTaskTerminal",
    "flushTerminalRefresh",
    "load",
    "rootAction",
    "addRoot",
  ]);
  const declarations = app.body.statements.filter((node) => {
    if (ts.isFunctionDeclaration(node)) return names.has(node.name?.text);
    return (
      ts.isVariableStatement(node) &&
      node.declarationList.declarations.some((declaration) => {
        const name = declaration.name;
        return (
          (ts.isIdentifier(name) &&
            ["terminalRefresh", "terminalRead"].includes(name.text)) ||
          (ts.isArrayBindingPattern(name) &&
            name.elements.some(
              (element) =>
                element.name?.getText(ast) === "resourceScanGeneration",
            ))
        );
      })
    );
  });
  const calls = [],
    applied = [],
    errors = [],
    notices = [],
    rootWorking = [],
    context = {
      React: { useRef: (current) => ({ current }) },
      useState(initial) {
        let value = initial;
        context.readGeneration = () => value;
        return [value, (next) => (value = next(value))];
      },
      ...lifecycle.exports,
      viewContext: {
        current: { rootId: "root-a", rootKey: "root-a:/synthetic/root-a" },
      },
      contextKey: { current: "root-a:/synthetic/root-a" },
      rootWorkingRef: { current: false },
      stateRequest: { current: 0 },
      rootDialog: null,
      api: async (command, args) => {
        calls.push({ command, args });
        return reply(command, args);
      },
      applyState(next) {
        applied.push(next);
        const rootId = next.settings.root_id,
          rootKey = `${rootId}:${next.settings.root}`;
        context.viewContext.current = { rootId, rootKey };
        context.contextKey.current = rootKey;
      },
      setError: (error) => errors.push(error),
      setRootError: (error) => errors.push(error),
      setRootWorking: (value) => rootWorking.push(value),
      setRootMenu() {},
      setRootDialog() {},
      notify: (text) => notices.push(text),
      t: (key) => key,
      readGeneration: () => 0,
    };
  const code = ts.transpileModule(
    declarations.map((node) => node.getText(ast)).join("\n") +
      "\nreturn {handleTaskTerminal, flushTerminalRefresh, load, rootAction, addRoot, pending: () => terminalRefresh.current, reading: () => terminalRead.current};",
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText;
  const actions = new Function(...Object.keys(context), code)(
    ...Object.values(context),
  );
  return { ...actions, calls, applied, errors, notices, rootWorking, context };
}

test("an install completed during the root chooser refreshes after cancellation", async () => {
  const choice = deferred(),
    app = appHandlers((command) =>
      command === "root_pick" ? choice.promise : snapshot(),
    ),
    choosing = app.addRoot();
  app.handleTaskTerminal(task());
  await flush();
  assert.deepEqual(
    app.calls.map((call) => call.command),
    ["root_pick"],
  );
  assert.equal(app.pending(), true);
  choice.resolve({ status: "cancelled" });
  await choosing;
  await flush();
  assert.deepEqual(
    app.calls.map((call) => call.command),
    ["root_pick", "bootstrap"],
  );
  assert.equal(app.applied.length, 1);
  assert.equal(app.pending(), false);
  assert.deepEqual(app.rootWorking, [true, false]);
});

for (const outcome of ["unavailable", "error"]) {
  test(`a deferred chooser ${outcome} also drains terminal refresh`, async () => {
    const choice = deferred(),
      app = appHandlers((command) =>
        command === "root_pick" ? choice.promise : snapshot(),
      ),
      choosing = app.addRoot();
    app.handleTaskTerminal(task());
    await flush();
    if (outcome === "error") choice.reject(Error("Synthetic chooser failure"));
    else
      choice.resolve({
        status: "unavailable",
        message: "Synthetic unavailable",
      });
    await choosing;
    await flush();
    assert.deepEqual(
      app.calls.map((call) => call.command),
      ["root_pick", "bootstrap"],
    );
    assert.equal(app.applied.length, 1);
    assert.equal(app.notices.length, 1);
    assert.equal(app.pending(), false);
  });
}

for (const failedCommand of ["root_register", "root_select"]) {
  test(`a chooser's ${failedCommand} failure retains and drains the refresh`, async () => {
    const blocked = deferred(),
      app = appHandlers((command) => {
        if (command === "root_pick")
          return { status: "selected", path: "/synthetic/root-b" };
        if (command === failedCommand) return blocked.promise;
        if (command === "root_register")
          return { root: { id: "root-b" }, bootstrap: snapshot() };
        return snapshot();
      }),
      choosing = app.addRoot();
    await flush();
    app.handleTaskTerminal(task());
    await flush();
    assert.equal(
      app.calls.filter((call) => call.command === "bootstrap").length,
      0,
    );
    blocked.reject(Error("Synthetic root operation failure"));
    await choosing;
    await flush();
    assert.equal(
      app.calls.filter((call) => call.command === "bootstrap").length,
      1,
    );
    assert.equal(app.notices.length, 1);
    assert.equal(app.pending(), false);
    assert.equal(app.context.rootWorkingRef.current, false);
  });
}

test("rootAction success coalesces global recovery terminals and reads the newly selected root", async () => {
  const rootReply = deferred(),
    app = appHandlers((command) =>
      command === "root_select"
        ? rootReply.promise
        : snapshot("root-b", { marker: "fresh" }),
    ),
    selecting = app.rootAction("root_select", { id: "root-b" });
  app.handleTaskTerminal(task());
  app.handleTaskTerminal(
    task({
      task_id: "recovery-c",
      kind: "resource_update",
      root_id: "root-c",
      stage: "error",
    }),
  );
  await flush();
  assert.equal(app.pending(), true);
  rootReply.resolve(snapshot("root-b", { marker: "root-action" }));
  await selecting;
  await flush();
  assert.deepEqual(
    app.calls.map((call) => call.command),
    ["root_select", "bootstrap"],
  );
  assert.deepEqual(
    app.applied.map((value) => value.marker),
    ["root-action", "fresh"],
  );
  assert.equal(app.context.viewContext.current.rootId, "root-b");
});

test("rootAction failure drains a pending terminal refresh", async () => {
  const rootReply = deferred(),
    app = appHandlers((command) =>
      command === "root_select" ? rootReply.promise : snapshot(),
    ),
    selecting = app.rootAction("root_select", { id: "root-b" });
  app.handleTaskTerminal(task());
  await flush();
  rootReply.reject(Error("Synthetic selection failure"));
  await selecting;
  await flush();
  assert.deepEqual(
    app.calls.map((call) => call.command),
    ["root_select", "bootstrap"],
  );
  assert.equal(app.applied.length, 1);
  assert.deepEqual(app.notices, ["Error: Synthetic selection failure"]);
  assert.equal(app.pending(), false);
});

test("successful addRoot drains a global recovery that finished after chooser selection", async () => {
  const rootReply = deferred(),
    app = appHandlers((command) => {
      if (command === "root_pick")
        return { status: "selected", path: "/synthetic/root-b" };
      if (command === "root_register")
        return { root: { id: "root-b" }, bootstrap: snapshot() };
      if (command === "root_select") return rootReply.promise;
      return snapshot("root-b", { marker: "fresh" });
    }),
    choosing = app.addRoot();
  await flush();
  app.handleTaskTerminal(task({ kind: "instance_import", root_id: "root-c" }));
  await flush();
  rootReply.resolve(snapshot("root-b", { marker: "root-action" }));
  await choosing;
  await flush();
  assert.deepEqual(
    app.calls.map((call) => call.command),
    ["root_pick", "root_register", "root_select", "bootstrap"],
  );
  assert.equal(app.applied.at(-1).marker, "fresh");
  assert.equal(app.context.viewContext.current.rootId, "root-b");
});

test("multiple same-turn terminals start one bootstrap", async () => {
  const app = appHandlers();
  app.handleTaskTerminal(task());
  app.handleTaskTerminal(task({ task_id: "install-b", root_id: "root-b" }));
  app.handleTaskTerminal(
    task({
      task_id: "recovery-c",
      kind: "instance_import",
      root_id: "root-c",
      stage: "error",
    }),
  );
  await flush();
  assert.deepEqual(
    app.calls.map((call) => call.command),
    ["bootstrap"],
  );
  assert.equal(app.pending(), false);
});

test("terminals arriving during a terminal read coalesce into one subsequent read", async () => {
  const first = deferred(),
    second = deferred(),
    replies = [first, second],
    app = appHandlers(() => replies.shift().promise);
  app.handleTaskTerminal(task());
  await flush();
  app.handleTaskTerminal(task({ task_id: "install-b" }));
  await flush();
  app.handleTaskTerminal(task({ task_id: "install-c" }));
  await flush();
  assert.equal(app.calls.length, 1);
  assert.equal(app.pending(), true);
  first.resolve(snapshot("root-a", { marker: "old" }));
  await flush();
  assert.equal(app.calls.length, 2);
  second.resolve(snapshot("root-a", { marker: "new" }));
  await flush();
  assert.deepEqual(
    app.applied.map((value) => value.marker),
    ["old", "new"],
  );
  assert.equal(app.pending(), false);
  assert.equal(app.reading(), null);
});

test("a root switch fences a bootstrap response from the previous root", async () => {
  const oldRead = deferred();
  let reads = 0;
  const app = appHandlers((command) => {
    if (command === "bootstrap" && ++reads === 1) return oldRead.promise;
    return snapshot("root-b");
  });
  app.handleTaskTerminal(task());
  await flush();
  await app.rootAction("root_select", { id: "root-b" });
  oldRead.resolve(snapshot("root-a"));
  await flush();
  assert.deepEqual(
    app.applied.map((value) => value.settings.root_id),
    ["root-b", "root-b"],
  );
  assert.equal(reads, 2);
  assert.equal(app.reading(), null);
});

for (const timing of [
  "before chooser cancellation",
  "after chooser cancellation",
]) {
  test(`a superseded terminal read finishing ${timing} retains its refresh`, async () => {
    const firstRead = deferred(),
      choice = deferred();
    let reads = 0;
    const app = appHandlers((command) => {
      if (command === "root_pick") return choice.promise;
      if (++reads === 1) return firstRead.promise;
      return snapshot("root-a", { marker: "fresh" });
    });
    app.handleTaskTerminal(task());
    await flush();
    const choosing = app.addRoot();
    if (timing === "before chooser cancellation") {
      firstRead.resolve(snapshot("root-a", { marker: "superseded" }));
      await flush();
      assert.equal(app.applied.length, 0);
      assert.equal(reads, 1);
    }
    choice.resolve({ status: "cancelled" });
    await choosing;
    await flush();
    if (timing === "after chooser cancellation") {
      assert.equal(reads, 1);
      firstRead.resolve(snapshot("root-a", { marker: "superseded" }));
      await flush();
    }
    assert.equal(reads, 2);
    assert.deepEqual(
      app.applied.map((value) => value.marker),
      ["fresh"],
    );
    assert.equal(app.pending(), false);
    assert.equal(app.reading(), null);
  });
}

test("a terminal read rejected during a chooser is replaced without exposing its stale error", async () => {
  const firstRead = deferred(),
    choice = deferred();
  let reads = 0;
  const app = appHandlers((command) => {
    if (command === "root_pick") return choice.promise;
    if (++reads === 1) return firstRead.promise;
    return snapshot("root-a", { marker: "fresh" });
  });
  app.handleTaskTerminal(task());
  await flush();
  const choosing = app.addRoot();
  firstRead.reject(Error("Synthetic obsolete read failure"));
  await flush();
  assert.equal(app.pending(), true);
  assert.equal(reads, 1);
  assert.deepEqual(app.errors, []);
  choice.resolve({ status: "cancelled" });
  await choosing;
  await flush();
  assert.equal(reads, 2);
  assert.equal(app.applied.at(-1).marker, "fresh");
  assert.equal(app.pending(), false);
  assert.equal(app.reading(), null);
});

for (const timing of ["before", "after"]) {
  test(`manual load taking pending terminal intent ignores its predecessor finishing ${timing} it`, async () => {
    const first = deferred(),
      second = deferred(),
      replies = [first, second],
      app = appHandlers(() => {
        assert.ok(replies.length, "No redundant terminal bootstrap may start");
        return replies.shift().promise;
      });
    app.handleTaskTerminal(task());
    await flush();
    const firstOwner = app.reading();
    app.handleTaskTerminal(task({ task_id: "install-b" }));
    await flush();
    assert.equal(app.calls.length, 1);
    const manual = app.load();
    assert.notEqual(app.reading(), firstOwner);
    assert.equal(app.pending(), false);
    if (timing === "before") {
      first.resolve(snapshot("root-a", { marker: "old" }));
      await flush();
      assert.equal(app.calls.length, 2);
      assert.equal(app.pending(), false);
    }
    second.resolve(snapshot("root-a", { marker: "fresh" }));
    await manual;
    if (timing === "after") {
      first.resolve(snapshot("root-a", { marker: "old" }));
      await flush();
    }
    await flush();
    assert.equal(app.calls.length, 2);
    assert.deepEqual(
      app.applied.map((value) => value.marker),
      ["fresh"],
    );
    assert.equal(app.pending(), false);
    assert.equal(app.reading(), null);
    assert.deepEqual(app.errors, []);
  });
}

test("an ordinary read retiring terminal work yields one bounded replacement", async () => {
  const first = deferred(),
    ordinary = deferred(),
    replacement = deferred(),
    replies = [first, ordinary, replacement],
    app = appHandlers(() => {
      assert.ok(replies.length, "Retired readers cannot create a refresh loop");
      return replies.shift().promise;
    });
  app.handleTaskTerminal(task());
  await flush();
  const manual = app.load();
  first.resolve(snapshot("root-a", { marker: "retired-terminal" }));
  await flush();
  assert.equal(app.calls.length, 3);
  replacement.resolve(snapshot("root-a", { marker: "fresh" }));
  await flush();
  ordinary.resolve(snapshot("root-a", { marker: "retired-ordinary" }));
  await manual;
  await flush();
  assert.equal(app.calls.length, 3);
  assert.deepEqual(
    app.applied.map((value) => value.marker),
    ["fresh"],
  );
  assert.equal(app.reading(), null);
  assert.equal(app.pending(), false);
  assert.deepEqual(app.errors, []);
});

test("an accepted terminal read failure drains later events once", async () => {
  const first = deferred();
  let reads = 0;
  const app = appHandlers(() => {
    if (++reads === 1) return first.promise;
    assert.equal(reads, 2, "Accepted failures must not create a retry loop");
    return snapshot();
  });
  app.handleTaskTerminal(task());
  await flush();
  app.handleTaskTerminal(task({ task_id: "install-b" }));
  app.handleTaskTerminal(task({ task_id: "install-c" }));
  await flush();
  assert.equal(reads, 1);
  first.reject(Error("Synthetic accepted failure"));
  await flush();
  assert.deepEqual(app.errors, ["Error: Synthetic accepted failure"]);
  assert.equal(reads, 2);
  assert.equal(app.applied.length, 1);
  assert.equal(app.pending(), false);
  assert.equal(app.reading(), null);
});

test("ordinary load blocked by the chooser does not consume a pending terminal", async () => {
  const choice = deferred(),
    app = appHandlers((command) =>
      command === "root_pick" ? choice.promise : snapshot(),
    ),
    choosing = app.addRoot();
  app.handleTaskTerminal(task());
  await app.load();
  await flush();
  assert.equal(app.pending(), true);
  assert.deepEqual(
    app.calls.map((call) => call.command),
    ["root_pick"],
  );
  choice.resolve({ status: "cancelled" });
  await choosing;
  await flush();
  assert.deepEqual(
    app.calls.map((call) => call.command),
    ["root_pick", "bootstrap"],
  );
});

test("ordinary load can satisfy a queued terminal refresh without a duplicate read", async () => {
  const app = appHandlers();
  app.handleTaskTerminal(task());
  await app.load();
  await flush();
  assert.equal(app.calls.length, 1);
  assert.equal(app.pending(), false);
});

test("a terminal bootstrap error is visible without losing a later terminal refresh", async () => {
  let reads = 0;
  const app = appHandlers(() => {
    if (++reads === 1) throw Error("Synthetic read failure");
    return snapshot();
  });
  app.handleTaskTerminal(task());
  await flush();
  assert.deepEqual(app.errors, ["Error: Synthetic read failure"]);
  await flush();
  assert.equal(app.calls.length, 1);
  assert.equal(app.pending(), false);
  assert.equal(app.reading(), null);
  app.handleTaskTerminal(task({ task_id: "install-b" }));
  await flush();
  assert.equal(app.calls.length, 2);
  assert.equal(app.applied.length, 1);
});

test("root operations without a task change do not add bootstrap reads", async () => {
  const app = appHandlers((command) =>
    command === "root_pick" ? { status: "cancelled" } : snapshot("root-b"),
  );
  await app.addRoot();
  await app.rootAction("root_select", { id: "root-b" });
  await flush();
  assert.deepEqual(
    app.calls.map((call) => call.command),
    ["root_pick", "root_select"],
  );
});

test("resource scan invalidation requires a current-root resource writer's terminal", () => {
  const { taskNeedsResourceScan } = lifecycle.exports,
    kinds = [
      "resource_download",
      "resource_update",
      "resource_update_restore",
      "resource_operation",
    ],
    stages = [
      "queued",
      "preparing",
      "downloading",
      "processing",
      "complete",
      "error",
      "cancelled",
    ];
  for (const kind of kinds)
    for (const stage of stages) {
      const status = task({ kind, stage }),
        terminal = ["complete", "error", "cancelled"].includes(stage);
      assert.equal(
        taskNeedsResourceScan(status, "root-a"),
        terminal,
        `${kind}/${stage}`,
      );
      assert.equal(taskNeedsResourceScan(status, "root-b"), false);
      assert.equal(taskNeedsResourceScan(status, null), false);
      assert.equal(
        taskNeedsResourceScan({ ...status, root_id: null }, null),
        false,
      );
    }
  for (const kind of [
    "install",
    "modpack_install",
    "modpack_prepare",
    "java_install",
    "instance_reset",
    "instance_repair",
    "instance_export",
    "instance_rename",
    "instance_import",
    "instance_delete",
    "instance_restore",
    "resource_save",
    "launcher_logs",
    "toolbox_download",
    null,
    undefined,
  ])
    for (const stage of stages)
      assert.equal(
        taskNeedsResourceScan(task({ kind, stage }), "root-a"),
        false,
        `${kind}/${stage}`,
      );
});

test("the App advances resource scan generation only at relevant terminal handlers", async () => {
  const app = appHandlers();
  for (const stage of ["queued", "preparing", "downloading", "processing"])
    app.handleTaskTerminal(task({ kind: "resource_download", stage }));
  app.handleTaskTerminal(task());
  app.handleTaskTerminal(task({ kind: "resource_save" }));
  app.handleTaskTerminal(task({ kind: "resource_update", root_id: "root-b" }));
  assert.equal(app.context.readGeneration(), 0);
  for (const stage of ["complete", "error", "cancelled"])
    app.handleTaskTerminal(
      task({ task_id: stage, kind: "resource_operation", stage }),
    );
  assert.equal(app.context.readGeneration(), 3);
  await flush();
  assert.equal(app.calls.length, 1);
});

test("InstancePanel receives a resource key based on root and terminal generation", () => {
  let expression;
  function visit(node) {
    if (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) {
      if (node.tagName.getText(ast) === "InstancePanel") {
        const attribute = node.attributes.properties.find(
          (value) => value.name?.getText(ast) === "resourceRefreshKey",
        );
        expression = attribute?.initializer?.expression?.getText(ast);
      }
    }
    ts.forEachChild(node, visit);
  }
  visit(ast);
  assert.ok(expression, "InstancePanel resource refresh key is connected");
  const key = new Function(
    "rootKey",
    "resourceScanGeneration",
    `return ${expression};`,
  );
  assert.equal(
    key("root-a:/synthetic/root-a", 0),
    "root-a:/synthetic/root-a:0",
  );
  assert.notEqual(
    key("root-a:/synthetic/root-a", 0),
    key("root-a:/synthetic/root-a", 1),
  );
  assert.notEqual(
    key("root-a:/synthetic/root-a", 1),
    key("root-b:/synthetic/root-b", 1),
  );
});
