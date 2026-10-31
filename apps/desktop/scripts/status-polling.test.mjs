/** Actual App effects and status poller, with deterministic clocks and native
 * replies. No desktop, accounts, game process or network provider is started. */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
const transpile = (source) =>
  ts.transpileModule(source, {
    compilerOptions: {
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.CommonJS,
    },
  }).outputText;
const status = { exports: {} };
new Function(
  "exports",
  transpile(
    readFileSync(new URL("../src/statusPolling.ts", import.meta.url), "utf8"),
  ),
)(status.exports);
const source = readFileSync(
  new URL("../src/main.tsx", import.meta.url),
  "utf8",
);
const ast = ts.createSourceFile(
  "main.tsx",
  source,
  ts.ScriptTarget.Latest,
  true,
  ts.ScriptKind.TSX,
);
const app = ast.statements.find(
  (n) => ts.isFunctionDeclaration(n) && n.name?.text === "App",
);
const effects = app.body.statements
  .filter(
    (n) =>
      ts.isExpressionStatement(n) &&
      ts.isCallExpression(n.expression) &&
      n.expression.expression.getText(ast) === "useEffect",
  )
  .map((n) => n.expression.arguments[0].getText(ast));
const pollingEffect = effects.find((text) =>
  text.includes("createStatusPoller"),
);
const rescheduleEffect = effects.find((text) =>
  text.includes("statusPolling.current?.process.reschedule"),
);
assert.ok(pollingEffect && rescheduleEffect);
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((a, b) => {
    resolve = a;
    reject = b;
  });
  return { promise, resolve, reject };
};
const flush = () => new Promise(setImmediate);
function initial() {
  return {
    status: {
      stage: "idle",
      message: "",
      version: null,
      pid: null,
      exit_code: null,
      root_id: null,
      root_path: null,
    },
    auth: {
      client_id: "fixture",
      selected: null,
      accounts: [],
      stage: "idle",
      message: "",
      challenge: null,
      warning: null,
      saveable: [],
      saving: false,
    },
  };
}
function driver(reply, state = initial()) {
  let now = 0,
    nextTimer = 0,
    changes = 0,
    cleanup;
  const timers = new Map(),
    calls = [],
    listeners = new Map(),
    pending = {},
    peak = {};
  const context = {
    ...status.exports,
    tasks: { closing: false },
    pollingState: { current: { data: state, authWorking: false } },
    statusPolling: { current: null },
    statusReadSlots: {
      current: { process: { current: null }, auth: { current: null } },
    },
    async api(command, args) {
      calls.push({ command, args, at: now });
      pending[command] = (pending[command] || 0) + 1;
      peak[command] = Math.max(peak[command] || 0, pending[command]);
      try {
        return reply
          ? await reply(command, args)
          : structuredClone(
              command === "process_status" ? state.status : state.auth,
            );
      } finally {
        pending[command]--;
      }
    },
    setData(update) {
      const next = update(state);
      if (next !== state) changes++;
      const previous = state;
      state = next;
      context.pollingState.current.data = state;
      if (
        previous.status.stage !== state.status.stage ||
        previous.auth.stage !== state.auth.stage ||
        previous.auth.saving !== state.auth.saving
      )
        reschedule();
    },
  };
  function events(prefix) {
    return {
      addEventListener(type, fn) {
        listeners.set(`${prefix}:${type}`, fn);
      },
      removeEventListener(type, fn) {
        if (listeners.get(`${prefix}:${type}`) === fn)
          listeners.delete(`${prefix}:${type}`);
      },
    };
  }
  context.window = {
    ...events("window"),
    setTimeout(fn, ms) {
      const id = ++nextTimer;
      timers.set(id, { fn, at: now + ms });
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
  };
  context.document = { ...events("document"), visibilityState: "visible" };
  const evaluate = (text) =>
    new Function(...Object.keys(context), transpile(`return (${text})();`))(
      ...Object.values(context),
    );
  const reschedule = () => evaluate(rescheduleEffect);
  cleanup = evaluate(pollingEffect);
  return {
    calls,
    peak,
    context,
    get state() {
      return state;
    },
    get changes() {
      return changes;
    },
    get timerCount() {
      return timers.size;
    },
    get listenerCount() {
      return listeners.size;
    },
    async advance(ms) {
      const target = now + ms;
      for (;;) {
        const due = [...timers.entries()]
          .filter(([, t]) => t.at <= target)
          .sort((a, b) => a[1].at - b[1].at)[0];
        if (!due) break;
        now = due[1].at;
        timers.delete(due[0]);
        due[1].fn();
        await flush();
      }
      now = target;
      await flush();
    },
    event(type) {
      listeners.get(type)?.();
    },
    working(value) {
      context.pollingState.current.authWorking = value;
      reschedule();
    },
    restart(closing) {
      cleanup?.();
      context.tasks.closing = closing;
      cleanup = evaluate(pollingEffect);
    },
    action(name, extras = {}) {
      const declaration = app.body.statements.find(
        (n) => ts.isFunctionDeclaration(n) && n.name?.text === name,
      );
      const bindings = { ...context, ...extras };
      return new Function(
        ...Object.keys(bindings),
        transpile(`${declaration.getText(ast)}; return ${name};`),
      )(...Object.values(bindings));
    },
    close() {
      cleanup?.();
    },
  };
}

test("actual App idle effect reads eight snapshots per minute and commits no duplicate state", async (t) => {
  const f = driver();
  t.after(() => f.close());
  await f.advance(60_000);
  assert.equal(f.calls.filter((c) => c.command === "process_status").length, 4);
  assert.equal(f.calls.filter((c) => c.command === "auth_status").length, 4);
  assert.equal(f.changes, 0);
  assert.equal(f.timerCount, 2);
  assert.deepEqual(f.peak, { process_status: 1, auth_status: 1 });
});
test("preparation/running and login/saving keep their original fast intervals", async (t) => {
  for (const stage of ["preparing", "running"]) {
    for (const authStage of ["preparing", "waiting", "saving"]) {
      const state = initial();
      state.status.stage = stage;
      state.auth.stage = authStage;
      const f = driver(undefined, state);
      t.after(() => f.close());
      await f.advance(9_000);
      assert.equal(
        f.calls.filter((c) => c.command === "process_status").length,
        10,
      );
      assert.equal(
        f.calls.filter((c) => c.command === "auth_status").length,
        9,
      );
      assert.equal(f.changes, 0);
    }
  }
});
test("focus and return to visibility wake idle reads; hidden changes do not", async (t) => {
  const f = driver();
  t.after(() => f.close());
  f.context.document.visibilityState = "hidden";
  f.event("document:visibilitychange");
  await flush();
  assert.equal(f.calls.length, 0);
  f.context.document.visibilityState = "visible";
  f.event("document:visibilitychange");
  await flush();
  assert.equal(f.calls.length, 2);
  f.event("window:focus");
  await flush();
  assert.equal(f.calls.length, 4);
});
test("slow reads and many wakes stay single-flight; closing/replacement reject late state", async (t) => {
  const old = deferred();
  let slow = true;
  const f = driver((command) =>
    slow
      ? old.promise
      : structuredClone(
          command === "process_status" ? initial().status : initial().auth,
        ),
  );
  t.after(() => f.close());
  f.working(true);
  await f.advance(1_000);
  for (let i = 0; i < 100; i++) f.event("window:focus");
  await f.advance(60_000);
  assert.equal(f.calls.length, 2);
  f.restart(true);
  assert.equal(f.timerCount, 0);
  assert.equal(f.listenerCount, 0);
  f.restart(false);
  f.event("window:focus");
  await flush();
  assert.equal(f.calls.length, 2);
  slow = false;
  old.resolve({ stage: "must-not-apply" });
  await flush();
  assert.equal(f.state.auth.stage, "idle");
  assert.equal(f.state.status.stage, "idle");
  assert.deepEqual(f.peak, { auth_status: 1, process_status: 1 });
  assert.equal(f.changes, 0);
  assert.equal(f.calls.length, 4);
});
test("auth operations await a fresh serial reply and cancellation wakes immediately", async (t) => {
  const stale = deferred();
  let reads = 0;
  const updated = initial().auth;
  updated.message = "operation complete";
  const f = driver((command) =>
    command === "auth_status"
      ? ++reads === 1
        ? stale.promise
        : structuredClone(updated)
      : {},
  );
  t.after(() => f.close());
  f.context.statusPolling.current.auth.wake();
  await flush();
  const extras = {
    setAuthWorking: (value) => f.working(value),
    setAuthError: (e) => {
      if (e) assert.fail(String(e));
    },
  };
  const operation = f.action("authAction", extras)("auth_start", {
    remember: false,
  });
  await flush();
  assert.equal(reads, 1);
  stale.resolve({ ...initial().auth, message: "stale" });
  await operation;
  assert.equal(reads, 2);
  assert.equal(f.state.auth.message, "operation complete");
  assert.equal(f.changes, 1);
  assert.equal(f.peak.auth_status, 1);
  let dialog;
  f.action("closeAccountDialog", {
    native: true,
    setDialog: (value) => {
      dialog = value;
    },
  })();
  await flush();
  assert.equal(dialog, null);
  assert.equal(reads, 3);
});
test("explicit refresh reports native errors while periodic failures retry", async (t) => {
  const f = driver(() => Promise.reject(new Error("synthetic unavailable")));
  t.after(() => f.close());
  await assert.rejects(
    f.context.statusPolling.current.auth.refresh(),
    /synthetic unavailable/,
  );
  await f.advance(30_000);
  assert.equal(f.timerCount, 2);
  assert.equal(f.changes, 0);
});
test("closing admission rejects replies even before effect cleanup runs", async (t) => {
  const pending = deferred();
  const f = driver(() => pending.promise);
  t.after(() => f.close());
  f.event("window:focus");
  await flush();
  f.context.pollingState.current.closing = true;
  pending.resolve({
    ...initial().auth,
    ...initial().status,
    message: "late reply",
  });
  await flush();
  assert.equal(f.changes, 0);
});
test("offline save refreshes auth immediately through the same owner", async (t) => {
  const updated = initial().auth;
  updated.message = "offline selected";
  const f = driver((command) =>
    command === "auth_status" ? structuredClone(updated) : {},
  );
  t.after(() => f.close());
  let saved, dialog;
  await f.action("saveOfflineAccount", {
    data: { settings: { player: "before" } },
    save: async (value) => {
      saved = value;
    },
    setAuthWorking: (value) => f.working(value),
    setAuthError: (error) => {
      if (error) assert.fail(String(error));
    },
    setDialog: (value) => {
      dialog = value;
    },
  })("after");
  assert.equal(saved.player, "after");
  assert.equal(dialog, null);
  assert.equal(f.state.auth.message, "offline selected");
  assert.deepEqual(
    f.calls.map((c) => c.command),
    ["auth_select", "auth_status"],
  );
});
test("a local game launch wakes the process poll without the idle delay", async (t) => {
  const updated = {
    ...initial().status,
    stage: "preparing",
    message: "actual preparation",
  };
  const f = driver((command) =>
    command === "process_status" ? structuredClone(updated) : {},
  );
  t.after(() => f.close());
  const calls = [];
  await f.action("launch", {
    data: { ...initial(), settings: { root: "/synthetic", player: "fixture" } },
    selected: { id: "fixture" },
    busy: false,
    downloadBusy: false,
    resourceBusy: false,
    rootWorking: false,
    rootAvailable: true,
    rootKey: "fixture",
    rootId: "root",
    contextKey: { current: "fixture" },
    save: async () => {},
    rootApi: async (command) => {
      calls.push(command);
    },
    t: (key) => key,
    notify: (error) => assert.fail(String(error)),
  })();
  await flush();
  assert.deepEqual(calls, ["launch_game"]);
  assert.equal(f.calls[0].command, "process_status");
  assert.equal(f.state.status.message, "actual preparation");
  await f.advance(900);
  assert.equal(f.calls.filter((c) => c.command === "process_status").length, 2);
});
test("all visible status/auth fields participate in equality, including nested textures", () => {
  const { sameProcessStatus, sameAuthState } = status.exports;
  const state = initial();
  for (const key of Object.keys(state.status)) {
    const next = {
      ...state.status,
      [key]: key === "pid" || key === "exit_code" ? 1 : "changed",
    };
    assert.equal(sameProcessStatus(state.status, next), false, key);
  }
  const auth = {
    ...state.auth,
    selected: "a",
    challenge: {
      user_code: "code",
      verification_uri: "https://invalid.example",
      expires_in: 60,
    },
    saveable: ["a"],
    accounts: [
      {
        remembered: true,
        profile: {
          id: "a",
          uuid: "uuid",
          name: "name",
          expires_at: 1,
          textures: {
            skin: { url: "skin", name: "skin" },
            cape: { url: "cape", name: "cape" },
          },
        },
      },
    ],
  };
  assert.equal(sameAuthState(auth, structuredClone(auth)), true);
  const paths = [
    ...["client_id", "selected", "stage", "message", "warning", "saving"].map(
      (key) => [key],
    ),
    ...["user_code", "verification_uri", "expires_in"].map((key) => [
      "challenge",
      key,
    ]),
    ["saveable", 0],
    ["accounts", 0, "remembered"],
    ...["id", "uuid", "name", "expires_at"].map((key) => [
      "accounts",
      0,
      "profile",
      key,
    ]),
    ...["skin", "cape"].flatMap((part) =>
      ["url", "name"].map((key) => [
        "accounts",
        0,
        "profile",
        "textures",
        part,
        key,
      ]),
    ),
  ];
  for (const path of paths) {
    const next = structuredClone(auth);
    let object = next;
    for (const key of path.slice(0, -1)) object = object[key];
    const key = path.at(-1),
      value = object[key];
    object[key] =
      typeof value === "boolean"
        ? !value
        : typeof value === "number"
          ? value + 1
          : "changed";
    assert.equal(sameAuthState(auth, next), false, path.join("."));
  }
  for (const path of [["challenge"], ["accounts"], ["saveable"]]) {
    const next = structuredClone(auth);
    next[path[0]] = path[0] === "challenge" ? null : [];
    assert.equal(sameAuthState(auth, next), false);
  }
});
