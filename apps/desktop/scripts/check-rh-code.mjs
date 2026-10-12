/** Exercise real Cordis lifetimes, manifest authority and standalone view hosts.
 * Scratch lives in the repository; no launcher, native host or AI is contacted. */
import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url),
  esbuild = require("esbuild");
const scratch = new URL("../../../work/rh-code-check/", import.meta.url)
  .pathname;
await mkdir(scratch, { recursive: true });
const dir = await mkdtemp(path.join(scratch, "registry-"));
try {
  await esbuild.build({
    entryPoints: [
      "kernel.ts",
      "layout.ts",
      "plugins/catalog.ts",
      "plugins/overview.plugin.tsx",
      "plugins/settings.plugin.tsx",
      "plugins/agent.plugin.tsx",
    ].map(
      (file) => new URL(`../src/rh-code/${file}`, import.meta.url).pathname,
    ),
    bundle: true,
    platform: "node",
    format: "esm",
    outdir: dir,
    outbase: new URL("../src/rh-code/", import.meta.url).pathname,
    outExtension: { ".js": ".mjs" },
    jsx: "automatic",
  });
  const read = (file) => import(pathToFileURL(path.join(dir, file)));
  const { Kernel } = await read("kernel.mjs"),
    layout = await read("layout.mjs"),
    { assemblePlugins } = await read("plugins/catalog.mjs"),
    overview = await read("plugins/overview.plugin.mjs"),
    settings = (await read("plugins/settings.plugin.mjs")).default,
    agent = (await read("plugins/agent.plugin.mjs")).default;
  const manifest = (id, dependencies = [], declarations = {}) => ({
    id,
    name: id,
    version: "1",
    required: false,
    dependencies,
    views: [],
    capabilities: [],
    ...declarations,
  });
  const view = (id, owner) => ({
    id,
    owner,
    label: id,
    render: () => null,
    icon: () => null,
  });
  let effects = 0,
    kernel,
    notices = 0;
  const leaf = {
    manifest: manifest("leaf", ["middle"], {
      views: ["custom.dynamic"],
      commands: ["leaf.answer"],
      layouts: ["leaf.layout"],
    }),
    views: [view("custom.dynamic", "leaf")],
    layouts: [
      {
        id: "leaf.layout",
        owner: "leaf",
        label: "Custom",
        create: () => layout.area("custom.dynamic"),
      },
    ],
    apply: (ctx) => {
      ctx.effect(() => {
        effects++;
        return () => {
          effects--;
        };
      });
      kernel.registerCommand(ctx, "leaf.answer", () => 42);
    },
  };
  kernel = new Kernel([
    leaf,
    { manifest: manifest("middle", ["core"]) },
    { manifest: manifest("core") },
  ]);
  const unsubscribe = kernel.subscribe(() => notices++);
  await kernel.configure([]);
  assert.equal(kernel.views.get("custom.dynamic").owner, "leaf");
  assert.equal(
    kernel.layouts.get("leaf.layout").create({}).view,
    "custom.dynamic",
  );
  assert.equal(kernel.invoke("leaf.answer"), 42);
  assert.equal(effects, 1);
  assert.equal(kernel.statuses.get("leaf").state, "active");
  const retainedCommand = kernel.commands.get("leaf.answer");
  await kernel.configure(["core"]);
  assert.throws(() => retainedCommand(), /unavailable/);
  assert.equal(
    kernel.views.size + kernel.layouts.size + kernel.commands.size,
    0,
  );
  assert.equal(effects, 0);
  assert.equal(kernel.statuses.get("core").state, "disabled");
  assert.equal(kernel.statuses.get("leaf").state, "blocked");
  await kernel.configure([]);
  assert.equal(effects, 1);
  await kernel.reportFailure("core", new Error("host render failure"));
  assert.equal(
    kernel.views.size + kernel.layouts.size + kernel.commands.size,
    0,
  );
  assert.equal(effects, 0);
  assert.match(kernel.statuses.get("core").error, /host render failure/);
  await kernel.retry("core");
  assert.equal(kernel.invoke("leaf.answer"), 42);
  await kernel.dispose();
  assert.equal(effects, 0);
  assert.equal(
    kernel.views.size + kernel.layouts.size + kernel.commands.size,
    0,
  );
  assert.throws(() => kernel.invoke("leaf.answer"), /unavailable/);
  await kernel.configure([]);
  assert.equal(kernel.statuses.get("leaf").state, "stopped");
  assert.ok(notices > 0);
  unsubscribe();

  // Dependency omissions and cycles are visible and leave independent plugins usable.
  const blocked = new Kernel([
    { manifest: manifest("missing", ["absent"]) },
    { manifest: manifest("a", ["b"]) },
    { manifest: manifest("b", ["a"]) },
    { manifest: manifest("independent", [], { required: true }) },
  ]);
  await blocked.configure(["independent"]);
  for (const id of ["missing", "a", "b"])
    assert.equal(blocked.statuses.get(id).state, "blocked");
  assert.deepEqual(blocked.statuses.get("missing").dependencies, ["absent"]);
  assert.equal(blocked.statuses.get("independent").state, "active");
  await blocked.dispose();

  // No foreign owner, undeclared ID or root-context command reaches a registry.
  const rejected = new Kernel([
    {
      manifest: manifest("foreign", [], { views: ["foreign.view"] }),
      views: [view("foreign.view", "victim")],
    },
    {
      manifest: manifest("undeclared"),
      views: [view("undeclared.view", "undeclared")],
    },
    {
      manifest: manifest("badcommand"),
      apply: (ctx) => rejected.registerCommand(ctx, "unlisted", () => true),
    },
    {
      manifest: manifest("badlayout"),
      layouts: [
        {
          id: "unlisted",
          owner: "badlayout",
          label: "Bad",
          create: () => layout.area("x"),
        },
      ],
    },
    {
      manifest: manifest("valid", [], { views: ["valid.view"] }),
      views: [view("valid.view", "valid")],
    },
  ]);
  await rejected.configure([]);
  for (const id of ["foreign", "undeclared", "badcommand", "badlayout"])
    assert.equal(rejected.statuses.get(id).state, "failed");
  assert.equal(rejected.views.size, 1);
  assert.throws(
    () => rejected.registerCommand(rejected.context, "unlisted", () => 1),
    /Undeclared/,
  );
  await rejected.dispose();

  // Conflicting declared IDs fail their second owner without deleting the
  // first registration. Missing implementations and malformed manifests fail
  // before they can prevent a valid sibling from starting.
  const collisions = new Kernel([
    {
      manifest: manifest("first", [], {
        views: ["shared.view"],
        commands: ["shared.command"],
        layouts: ["shared.layout"],
      }),
      views: [view("shared.view", "first")],
      commands: [
        { id: "shared.command", owner: "first", handler: () => "first" },
      ],
      layouts: [
        {
          id: "shared.layout",
          owner: "first",
          label: "First",
          create: () => layout.area("shared.view"),
        },
      ],
    },
    {
      manifest: manifest("second", [], { views: ["shared.view"] }),
      views: [view("shared.view", "second")],
    },
    {
      manifest: manifest("secondcommand", [], { commands: ["shared.command"] }),
      commands: [
        {
          id: "shared.command",
          owner: "secondcommand",
          handler: () => "wrong",
        },
      ],
    },
    {
      manifest: manifest("secondlayout", [], { layouts: ["shared.layout"] }),
      layouts: [
        {
          id: "shared.layout",
          owner: "secondlayout",
          label: "Wrong",
          create: () => layout.area("wrong"),
        },
      ],
    },
    { manifest: manifest("missingview", [], { views: ["missing.view"] }) },
    {
      manifest: manifest("missingcommand", [], {
        commands: ["missing.command"],
      }),
    },
    { manifest: manifest("invaliddeps", null) },
    {
      manifest: manifest("invalidview", [], { views: ["view:invalid"] }),
      views: [view("view:invalid", "invalidview")],
    },
    {
      manifest: manifest("longview", [], { views: ["x".repeat(121)] }),
      views: [view("x".repeat(121), "longview")],
    },
  ]);
  await collisions.configure([]);
  for (const id of [
    "second",
    "secondcommand",
    "secondlayout",
    "missingview",
    "missingcommand",
    "invaliddeps",
    "invalidview",
    "longview",
  ])
    assert.equal(collisions.statuses.get(id).state, "failed");
  assert.equal(collisions.views.get("shared.view").owner, "first");
  assert.equal(collisions.layouts.get("shared.layout").owner, "first");
  assert.equal(collisions.invoke("shared.command"), "first");
  await collisions.configure(["first"]);
  assert.equal(collisions.views.get("shared.view").owner, "second");
  assert.equal(collisions.invoke("shared.command"), "wrong");
  await collisions.dispose();

  // Startup failure removes early contributions/effects and blocks descendants,
  // while a later configuration retries the owner and its descendants.
  let fail = true;
  const faulty = new Kernel([
    { manifest: manifest("dependent", ["bad"]) },
    {
      manifest: manifest("bad", [], {
        views: ["bad.view"],
        commands: ["bad.command"],
        layouts: ["bad.layout"],
      }),
      views: [view("bad.view", "bad")],
      commands: [{ id: "bad.command", owner: "bad", handler: () => 1 }],
      layouts: [
        {
          id: "bad.layout",
          owner: "bad",
          label: "Bad",
          create: () => layout.area("bad.view"),
        },
      ],
      apply: async (ctx) => {
        ctx.effect(() => {
          effects++;
          return () => {
            effects--;
          };
        });
        if (fail) throw new Error("broken module");
      },
    },
    { manifest: manifest("good") },
  ]);
  await faulty.configure([]);
  assert.equal(faulty.statuses.get("bad").state, "failed");
  assert.equal(faulty.statuses.get("dependent").state, "blocked");
  assert.equal(faulty.statuses.get("good").state, "active");
  assert.equal(
    faulty.views.size + faulty.layouts.size + faulty.commands.size,
    0,
  );
  assert.equal(effects, 0);
  fail = false;
  await faulty.retry("bad");
  assert.equal(faulty.statuses.get("dependent").state, "active");
  assert.equal(faulty.invoke("bad.command"), 1);
  assert.equal(effects, 1);
  await faulty.dispose();
  assert.equal(effects, 0);

  // Entries cannot replace shared manifest declarations; missing, duplicate and
  // thrown factories are isolated so other entries still load.
  const manifests = [
    manifest("new", [], { views: ["new.component"] }),
    manifest("missingentry"),
    manifest("forged"),
    manifest("duplicate"),
    manifest("throws"),
    manifest("mutates"),
  ];
  const component = view("new.component", "new");
  const assembled = assemblePlugins(manifests, [
    { id: "new", create: (manifest) => ({ manifest, views: [component] }) },
    {
      id: "forged",
      create: (manifest) => ({
        manifest: { ...manifest, views: ["forged.view"] },
      }),
    },
    { id: "duplicate", create: (manifest) => ({ manifest }) },
    { id: "duplicate", create: (manifest) => ({ manifest }) },
    {
      id: "throws",
      create: () => {
        throw new Error("factory failure");
      },
    },
    {
      id: "mutates",
      create: (manifest) => {
        manifest.views.push("foreign");
        return { manifest };
      },
    },
    {
      id: "unlisted",
      create: () => {
        throw new Error("must never run");
      },
    },
  ]);
  const dynamic = new Kernel(assembled);
  await dynamic.configure([]);
  assert.equal(dynamic.views.get("new.component").render, component.render);
  for (const id of ["missingentry", "forged", "duplicate", "throws", "mutates"])
    assert.equal(dynamic.statuses.get(id).state, "failed");
  assert.equal(dynamic.statuses.has("unlisted"), false);
  await dynamic.dispose();

  // Node tool scope declarations share the bundled manifest. Nested mode arrays
  // must not remain mutable through a component's copy of that manifest.
  const toolSource = manifest("tool-owner", [], {
    capabilities: ["project.files.read"],
    tools: [
      {
        name: "files.inspect",
        capability: "project.files.read",
        modes: ["ask"],
      },
    ],
  });
  const frozenTools = assemblePlugins(
    [toolSource],
    [{ id: "tool-owner", create: (manifest) => ({ manifest }) }],
  );
  assert.throws(
    () => frozenTools[0].manifest.tools[0].modes.push("execute"),
    TypeError,
  );
  assert.throws(() => {
    frozenTools[0].manifest.tools[0].capability = "project.files.write";
  }, TypeError);
  toolSource.tools[0].modes.push("execute");
  assert.deepEqual(frozenTools[0].manifest.tools[0].modes, ["ask"]);
  const toolKernel = new Kernel([
    ...frozenTools,
    {
      manifest: manifest("invalid-tool", [], {
        capabilities: ["project.files.read"],
        tools: [
          {
            name: "files.write",
            capability: "project.files.write",
            modes: ["execute"],
          },
        ],
      }),
    },
  ]);
  await toolKernel.configure([]);
  assert.equal(toolKernel.statuses.get("tool-owner").state, "active");
  assert.equal(toolKernel.statuses.get("invalid-tool").state, "failed");
  await toolKernel.dispose();

  // The production agent entry explicitly fails without its runtime handlers;
  // supplied handlers remain declared commands with normal lifecycle ownership.
  const agentManifest = manifest("rh.agent", [], {
    commands: ["agent.run", "agent.stop"],
  });
  const unavailableAgent = new Kernel([agent.create(agentManifest, {})]);
  await unavailableAgent.configure([]);
  assert.equal(unavailableAgent.statuses.get("rh.agent").state, "failed");
  assert.match(
    unavailableAgent.statuses.get("rh.agent").error,
    /Declared command missing/,
  );
  assert.equal(unavailableAgent.commands.size, 0);
  await unavailableAgent.dispose();
  const availableAgent = new Kernel([
    agent.create(agentManifest, {
      commands: { run: () => "run", stop: () => "stop" },
    }),
  ]);
  await availableAgent.configure([]);
  assert.equal(availableAgent.statuses.get("rh.agent").state, "active");
  assert.equal(availableAgent.invoke("agent.run"), "run");
  assert.equal(availableAgent.invoke("agent.stop"), "stop");
  await availableAgent.dispose();

  // The actual overview entry works with only ViewHost and preserves unrelated
  // view state through the host contract. Its setting changes rendered content.
  const project = {
    id: "p",
    name: "Fixture",
    folder: "/project",
    targets: [{ minecraft: "1.21", loaders: ["Fabric"] }],
    conversations: [],
    layout: null,
    updatedAt: 0,
  };
  const area = layout.area("rh.overview.summary");
  area.state = { "other.view": { retained: "value" } };
  let state = { custom: "retained" };
  const api = {
    project,
    get state() {
      return state;
    },
    updateState(next) {
      state = next;
      area.state = { ...area.state, [area.view]: next };
    },
    invoke() {
      throw new Error("overview must not invoke commands");
    },
  };
  const props = { area, api };
  assert.ok(JSON.stringify(overview.Overview(props)).includes("/project"));
  const setting = overview.OverviewSettings(props).props.children[0];
  setting.props.onChange({ target: { checked: false } });
  assert.equal(state.showFolder, false);
  assert.equal(state.custom, "retained");
  assert.equal(area.state["other.view"].retained, "value");
  assert.equal(
    JSON.stringify(overview.Overview(props)).includes("/project"),
    false,
  );
  const presets = settings.create(
    manifest("rh.settings", [], {
      layouts: [
        "rh.settings.conversation",
        "rh.settings.parallel",
        "rh.settings.source",
      ],
    }),
    {},
  ).layouts;
  assert.equal(presets.length, 3);
  assert.equal(
    layout.leaves(presets[1].create(project)).filter((v) => v.view === "chat")
      .length,
    2,
  );

  const a = layout.area("chat", "one"),
    b = layout.area("source");
  b.file = "Main.java";
  const tree = layout.split("row", a, b),
    swapped = layout.exchange(tree, a.id, b.id);
  assert.equal(swapped.children[0].file, "Main.java");
  assert.equal(swapped.children[1].conversation, "one");
  assert.equal(new Set(layout.leaves(swapped).map((v) => v.id)).size, 2);
  assert.equal(tree.children[0].conversation, "one");
  assert.equal(layout.remove(swapped, b.id).conversation, "one");
  assert.equal(layout.resize(tree, tree.id, 10).ratio, 0.85);
  let deep = layout.area("chat");
  for (let n = 0; n < 8; n++)
    deep = layout.split("row", deep, layout.area("source"));
  assert.equal(layout.depth(deep), 8);
  console.log(
    "RH Code: lifecycle, manifest ownership, transitive retirement, isolated failures/retry, dynamic entries, view settings and layout identity passed.",
  );
} finally {
  await rm(dir, { recursive: true, force: true });
}
