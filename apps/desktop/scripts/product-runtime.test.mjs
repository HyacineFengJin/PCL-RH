/** Scope ownership and IPC races in the real lightweight product bridge. */
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
const ts = createRequire(import.meta.url)("typescript");
const code = ts.transpileModule(
  readFileSync(
    new URL("../src/productPluginRuntime.ts", import.meta.url),
    "utf8",
  ),
  {
    compilerOptions: {
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.CommonJS,
    },
  },
).outputText;
const module = { exports: {} };
new Function("exports", code)(module.exports);
const { createProductPluginRuntime } = module.exports;
function deferred() {
  let resolve, reject;
  const promise = new Promise((a, b) => {
    resolve = a;
    reject = b;
  });
  return { promise, resolve, reject };
}
function fixture() {
  const calls = [],
    active = new Set(),
    opens = [],
    closes = [];
  let next = 0;
  const api = async (command, a) => {
    calls.push({ command, ...a });
    assert.equal(command, "rh_code_call");
    if (a.operation === "plugin_configuration")
      return { revision: "fixture", disabledPlugins: [] };
    if (a.operation === "native_plugin")
      return { id: a.args.id, state: "active" };
    assert.equal(a.operation, "plugin_runtime");
    if (a.args.action === "open") {
      const response = opens.shift();
      const data = response ? await response : { lease: `lease-${++next}` };
      active.add(data.lease);
      return data;
    }
    const response = closes.shift();
    if (response) await response;
    active.delete(a.args.lease);
    return { released: true };
  };
  const surface = { api, native: true };
  return {
    surface,
    calls,
    active,
    opens,
    closes,
    port: createProductPluginRuntime(() => surface),
  };
}
test("launcher startup reads only control metadata and imports no Code implementation", async () => {
  const f = fixture();
  assert.equal(code.includes("require("), false);
  await f.port.readConfiguration();
  await f.port.configure(["launcher"]);
  assert.deepEqual(
    f.calls.map((c) => c.operation),
    ["plugin_configuration"],
  );
  await f.port.dispose();
});
test("Code owns one lease across tab changes and releases it only at actual retirement", async () => {
  const f = fixture();
  await f.port.configure(["launcher", "rh-code"]);
  await f.port.configure(["launcher", "rh-code"]);
  assert.equal(f.active.size, 1);
  await f.port.configure(["launcher"]);
  assert.equal(f.active.size, 0);
  await f.port.configure(["launcher", "rh-code"]);
  assert.equal(f.active.size, 1);
  await f.port.dispose();
  assert.equal(f.active.size, 0);
  assert.equal(f.calls.filter((c) => c.args?.action === "open").length, 2);
});
test("disposal during an in-flight open releases the eventual exact lease", async () => {
  const f = fixture(),
    d = deferred();
  f.opens.push(d.promise);
  const opening = f.port.configure(["rh-code"]);
  await Promise.resolve();
  const disposing = f.port.dispose();
  d.resolve({ lease: "delayed-owner" });
  await Promise.all([opening, disposing]);
  assert.equal(f.active.size, 0);
  assert.deepEqual(
    f.calls.filter((c) => c.args?.action === "close").map((c) => c.args.lease),
    ["delayed-owner"],
  );
  await f.port.configure(["rh-code"]);
  assert.equal(f.active.size, 0);
  await assert.rejects(f.port.activate("fixture"), /retired/);
});
test("two host lifetimes never close each other's leases", async () => {
  const f = fixture(),
    other = createProductPluginRuntime(() => f.surface);
  await Promise.all([
    f.port.configure(["rh-code"]),
    other.configure(["rh-code"]),
  ]);
  assert.equal(f.active.size, 2);
  await f.port.dispose();
  assert.equal(f.active.size, 1);
  await f.port.dispose();
  assert.equal(f.active.size, 1);
  await other.dispose();
  assert.equal(f.active.size, 0);
});
test("failed opens do not poison later requests; failed release retains retry ownership", async () => {
  const f = fixture(),
    d = deferred();
  f.opens.push(d.promise);
  const opening = f.port.configure(["rh-code"]);
  d.reject(new Error("fixture open failure"));
  await assert.rejects(opening, /open failure/);
  await f.port.configure(["rh-code"]);
  assert.equal(f.active.size, 1);
  const c = deferred();
  f.closes.push(c.promise);
  const closing = f.port.dispose();
  c.reject(new Error("fixture close failure"));
  await assert.rejects(closing, /close failure/);
  assert.equal(f.active.size, 1);
  await f.port.dispose();
  assert.equal(f.active.size, 0);
});
test("cleanup uses the captured owner connection after the current API changes", async () => {
  const f = fixture();
  await f.port.configure(["rh-code"]);
  f.surface.api = async () => {
    throw new Error("new connection must not receive old cleanup");
  };
  await f.port.dispose();
  assert.equal(f.active.size, 0);
});
test("preview never opens a native scope and invalid activation replies fail explicitly", async () => {
  const f = fixture();
  f.surface.native = false;
  await f.port.configure(["rh-code"]);
  await f.port.activate("fixture");
  await f.port.dispose();
  assert.equal(f.calls.length, 0);
  const bad = createProductPluginRuntime(() => ({
    native: true,
    api: async () => ({ id: "another", state: "active" }),
  }));
  await assert.rejects(bad.activate("fixture"), /unavailable/);
  await bad.dispose();
});
