import assert from "node:assert/strict";
import { test } from "node:test";
import {
  communityHarness,
  elements,
  content,
  deferred,
  button,
} from "./community-test-harness.mjs";

const aliases = [
  {
    source: "CurseForge",
    project_id: "123",
    url: "https://www.curseforge.com/minecraft/mc-mods/example",
  },
  {
    source: "Modrinth",
    project_id: "Abcd1234",
    url: "https://modrinth.com/mod/Abcd1234",
  },
  {
    source: "MC百科",
    project_id: "456",
    url: "https://www.mcmod.cn/class/456.html",
  },
];
function summary(extra = {}) {
  return {
    project_id: "123",
    title: "Example",
    description: "Synthetic project",
    icon_url: null,
    categories: ["fabric"],
    display_categories: ["utility"],
    versions: ["1.21.1"],
    project_type: "mod",
    source: "CurseForge",
    url: aliases[0].url,
    identity_key: "synthetic:example",
    ...extra,
  };
}
function details(extra = {}) {
  return {
    project: {
      ...summary(extra),
      game_versions: ["1.21.1"],
      loaders: ["fabric"],
    },
    versions: [
      {
        id: "999",
        project_id: extra.project_id ?? "123",
        name: "Official file",
        version_number: "1.0",
        game_versions: ["1.21.1"],
        loaders: ["fabric"],
        date_published: "2026-01-01T00:00:00Z",
        downloads: 42,
        version_type: "release",
        files: [{ filename: "example.jar", size: 100, primary: true }],
        dependencies: [
          {
            project_id: "dependency",
            version_id: "foreign-id",
            file_name: null,
            dependency_type: "required",
          },
        ],
      },
    ],
    versions_truncated: false,
    ...extra,
  };
}
function catalog(extra = {}) {
  const calls = [],
    requests = [],
    opened = [];
  const api = (command, args) => {
    calls.push({ command, args });
    const r = deferred();
    requests.push(r);
    return r.promise;
  };
  const ui = communityHarness("CommunityCatalog", {
    api,
    label: "模组",
    query: "",
    setQuery(value) {
      ui.replace({ query: value });
    },
    onResourceDetails(value) {
      opened.push(value);
    },
    ...extra,
  });
  return { ui, calls, requests, opened };
}
function panel(extra = {}) {
  const calls = [],
    requests = [],
    opened = [],
    notices = [];
  const api = (command, args) => {
    calls.push({ command, args });
    const r = deferred();
    requests.push(r);
    return r.promise;
  };
  const ui = communityHarness("ResourceDetails", {
    api,
    resource: summary(),
    scopeKey: "root-a",
    native: true,
    disabled: false,
    selectedInstance: {
      id: "instance-a",
      minecraft_version: "1.21.1",
      loader: "fabric",
    },
    onNotify(value) {
      notices.push(value);
    },
    onResourceDetails(value) {
      opened.push(value);
    },
    onTaskStart() {},
    ...extra,
  });
  return { ui, calls, requests, opened, notices };
}
const rows = (ui) =>
  elements(
    ui.tree,
    (n) => n.type === "article" && n.props.className === "ce-mod-row",
  );
const sourceControls = (ui) =>
  elements(ui.tree, (n) => n.type?.name === "CeSelect");
const child = (ui, name) => elements(ui.tree, (n) => n.type?.name === name);
const clickVersion = async (ui) => {
  elements(
    ui.tree,
    (n) => n.type === "button" && n.props.className?.includes("rd-version-row"),
  )[0].props.onClick();
  await ui.flush();
};
const selectSource = (ui) =>
  elements(
    ui.tree,
    (n) =>
      n.type === "button" &&
      n.props.title?.startsWith(
        ui.i18n.t("community.sourceSwitch", { source: "Modrinth" }),
      ),
  )[0];

test("merged catalog rows retain verified identities and partial provider warnings", async () => {
  const { ui, calls, requests, opened } = catalog();
  assert.equal(calls[0].command, "community_search");
  requests[0].resolve({
    hits: [summary({ sources: aliases })],
    warnings: ["CurseForge is temporarily unavailable"],
  });
  await ui.flush();
  assert.equal(rows(ui).length, 1);
  assert.ok(content(ui.tree).includes("CurseForge is temporarily unavailable"));
  assert.equal(elements(ui.tree, (n) => n.props.role === "alert").length, 0);
  assert.equal(
    elements(ui.tree, (n) => n.props.className === "ce-community-badge").length,
    3,
  );
  rows(ui)[0].props.onClick();
  assert.equal(opened[0].source, "CurseForge");
  assert.equal(opened[0].identity_key, "synthetic:example");
  assert.deepEqual(opened[0].sources, aliases);
  const options = elements(sourceControls(ui)[0], (n) => n.type === "option");
  assert.equal(
    options.find((n) => n.props.value === "CurseForge").props.disabled,
    false,
  );
  assert.ok(options.find((n) => n.props.value === "MC百科"));
  ui.close();
});

test("encyclopedia disables incompatible filters and omits them from search", async () => {
  const { ui, calls } = catalog({
    compatibility: { version: "1.21.1", loader: "fabric" },
  });
  sourceControls(ui)[0].props.onChange({ target: { value: "MC百科" } });
  await ui.flush();
  const controls = sourceControls(ui);
  assert.equal(
    controls.slice(1).every((n) => n.props.disabled),
    true,
  );
  elements(ui.tree, (n) => n.type === "form")[0].props.onSubmit({
    preventDefault() {},
  });
  await ui.flush();
  assert.equal(calls.at(-1).args.source, "MC百科");
  for (const key of ["version", "loader", "tag", "sort"])
    assert.equal(calls.at(-1).args[key], undefined);
  ui.close();
});

test("encyclopedia is unavailable for resource types it cannot represent", () => {
  const { ui } = catalog({ label: "数据包" });
  const option = elements(
    sourceControls(ui)[0],
    (n) => n.type === "option" && n.props.value === "MC百科",
  )[0];
  assert.equal(option.props.disabled, true);
  ui.close();
});

test("catalog API changes retire old responses and stored row handlers", async () => {
  const { ui, requests, opened } = catalog();
  requests[0].resolve({ hits: [summary()], warnings: [] });
  await ui.flush();
  const oldRow = rows(ui)[0];
  const replacement = deferred();
  ui.replace({ api: () => replacement.promise });
  oldRow.props.onClick();
  assert.equal(opened.length, 0);
  replacement.resolve({
    hits: [summary({ project_id: "321", title: "Replacement" })],
    warnings: [],
  });
  await ui.flush();
  assert.ok(content(ui.tree).includes("Replacement"));
  ui.close();
});

test("CurseForge files save with provider identity and never show game installation or foreign dependencies", async () => {
  const { ui, calls, requests } = panel();
  assert.deepEqual(calls[0], {
    command: "community_details",
    args: { source: "CurseForge", projectId: "123", projectType: "mod" },
  });
  requests[0].resolve(details());
  await ui.flush();
  assert.equal(calls.length, 1, "CF dependencies must not be sent to Modrinth");
  await clickVersion(ui);
  assert.equal(button(ui, "resource.downloadFile").props.disabled, false);
  button(ui, "resource.downloadFile").props.onClick();
  await ui.flush();
  assert.deepEqual(child(ui, "ResourceSave")[0].props.request, {
    source: "CurseForge",
    project_id: "123",
    version_id: "999",
    file_name: "example.jar",
  });
  assert.equal(child(ui, "ResourceInstall").length, 0);
  assert.equal(child(ui, "ResourceFavorite").length, 0);
  assert.equal(
    elements(
      ui.tree,
      (n) =>
        n.type === "button" && content(n) === ui.i18n.t("resource.install"),
    ).length,
    0,
  );
  ui.close();
});

test("CurseForge modpacks also save a single official archive without Modrinth import", async () => {
  const { ui, requests } = panel({
    resource: summary({ project_type: "modpack" }),
  });
  const data = details();
  data.project.project_type = "modpack";
  data.versions[0].files[0].filename = "example.zip";
  requests[0].resolve(data);
  await ui.flush();
  await clickVersion(ui);
  button(ui, "resource.downloadFile").props.onClick();
  await ui.flush();
  assert.equal(child(ui, "ResourceSave")[0].props.request.source, "CurseForge");
  assert.equal(child(ui, "InstanceImport").length, 0);
  ui.close();
});

test("Modrinth keeps its own dependency, save and game installation IDs", async () => {
  const resource = summary({ source: "Modrinth", project_id: "Abcd1234" });
  const { ui, calls, requests } = panel({ resource });
  requests[0].resolve(details({ source: "Modrinth", project_id: "Abcd1234" }));
  await ui.flush();
  assert.equal(calls[1].command, "resource_dependencies");
  assert.equal(calls[1].args.versionId, "999");
  await clickVersion(ui);
  button(ui, "resource.downloadFile").props.onClick();
  await ui.flush();
  assert.deepEqual(child(ui, "ResourceSave")[0].props.request, {
    project_id: "Abcd1234",
    version_id: "999",
    file_name: "example.jar",
  });
  button(ui, "resource.install").props.onClick();
  await ui.flush();
  assert.equal(child(ui, "ResourceSave").length, 0);
  assert.deepEqual(child(ui, "ResourceInstall")[0].props.request, {
    project_id: "Abcd1234",
    version_id: "999",
    file_name: "example.jar",
  });
  ui.close();
});

test("Modrinth modpacks retain the original official import request", async () => {
  const { ui, requests } = panel({
    resource: summary({
      source: "Modrinth",
      project_id: "Abcd1234",
      project_type: "modpack",
    }),
  });
  const data = details({ source: "Modrinth", project_id: "Abcd1234" });
  data.project.project_type = "modpack";
  data.versions[0].files[0].filename = "example.mrpack";
  requests[0].resolve(data);
  await ui.flush();
  await clickVersion(ui);
  assert.deepEqual(child(ui, "InstanceImport")[0].props.official.request, {
    project_id: "Abcd1234",
    version_id: "999",
    file_name: "example.mrpack",
  });
  assert.equal(child(ui, "ResourceSave").length, 0);
  ui.close();
});

test("verified Modrinth mapping preserves favorite identity and source switches preserve aliases", async () => {
  const { ui, requests, opened } = panel({
    resource: summary({ sources: aliases }),
  });
  requests[0].resolve(details({ sources: aliases }));
  await ui.flush();
  assert.equal(child(ui, "ResourceFavorite")[0].props.projectId, "Abcd1234");
  selectSource(ui).props.onClick();
  assert.equal(opened[0].source, "Modrinth");
  assert.equal(opened[0].project_id, "Abcd1234");
  assert.equal(opened[0].identity_key, "synthetic:example");
  assert.deepEqual(opened[0].sources, aliases);
  ui.close();
});

test("URL-only aliases open the verified page and encyclopedia button uses its exact project", async () => {
  const urlOnly = { ...aliases[1], project_id: null };
  const { ui, calls, requests, opened } = panel({
    resource: summary({ sources: [aliases[0], urlOnly, aliases[2]] }),
  });
  requests[0].resolve(details());
  await ui.flush();
  const open = elements(
    ui.tree,
    (n) =>
      n.type === "button" &&
      n.props.title ===
        ui.i18n.t("community.sourceOpen", { source: "Modrinth" }),
  )[0];
  open.props.onClick();
  assert.equal(calls.at(-1).command, "resource_open_link");
  assert.equal(calls.at(-1).args.url, aliases[1].url);
  assert.equal(opened.length, 0);
  button(ui, "resource.mcmod").props.onClick();
  assert.equal(calls.at(-1).args.url, aliases[2].url);
  ui.close();
});

test("without encyclopedia alias the encyclopedia button searches the current project name", () => {
  const { ui, calls } = panel();
  button(ui, "resource.mcmod").props.onClick();
  assert.equal(calls.at(-1).args.url, "https://search.mcmod.cn/s?key=Example");
  ui.close();
});

test("encyclopedia details show references and attribution without download filters or files", async () => {
  const { ui, calls, requests } = panel({
    resource: summary({
      source: "MC百科",
      project_id: "456",
      url: aliases[2].url,
      sources: aliases,
    }),
  });
  const data = details({
    source: "MC百科",
    project_id: "456",
    versions: [],
    attribution: "MC百科 · CC BY-NC-SA 3.0",
  });
  requests[0].resolve(data);
  await ui.flush();
  assert.equal(calls.length, 1);
  assert.equal(
    elements(ui.tree, (n) =>
      n.props.className?.startsWith("ce-card rd-filters"),
    ).length,
    0,
  );
  assert.equal(child(ui, "ResourceSave").length, 0);
  assert.ok(content(ui.tree).includes("CC BY-NC-SA 3.0"));
  assert.ok(content(ui.tree).includes(ui.i18n.t("community.mcmodReference")));
  ui.close();
});

test("source changes reject stale detail replies even for colliding provider IDs", async () => {
  const { ui, requests } = panel();
  ui.replace({ resource: summary({ source: "Modrinth" }) });
  requests[1].resolve(details({ title: "Current provider" }));
  await ui.flush();
  requests[0].resolve(details({ title: "Stale provider" }));
  await ui.flush();
  assert.ok(content(ui.tree).includes("Current provider"));
  assert.ok(!content(ui.tree).includes("Stale provider"));
  ui.close();
});

test("root changes reject stale detail replies with the same project and API", async () => {
  const { ui, requests } = panel();
  ui.replace({ scopeKey: "root-b" });
  requests[0].resolve(details({ title: "Old root" }));
  await ui.flush();
  assert.ok(!content(ui.tree).includes("Old root"));
  requests[1].resolve(details({ title: "New root" }));
  await ui.flush();
  assert.ok(content(ui.tree).includes("New root"));
  ui.close();
});

test("source changes remove selected files and retire stored save and version handlers", async () => {
  const { ui, requests } = panel();
  requests[0].resolve(details());
  await ui.flush();
  await clickVersion(ui);
  const save = button(ui, "resource.downloadFile");
  const oldVersion = elements(
    ui.tree,
    (n) => n.type === "button" && n.props.className?.includes("rd-version-row"),
  )[0];
  ui.replace({ resource: summary({ source: "Modrinth" }) });
  save.props.onClick();
  oldVersion.props.onClick();
  await ui.flush();
  assert.equal(child(ui, "ResourceSave").length, 0);
  assert.equal(
    elements(
      ui.tree,
      (n) => n.props.className === "ce-card rd-version-group rd-selected",
    ).length,
    0,
  );
  ui.close();
});

function saver(extra = {}) {
  const calls = [],
    requests = [],
    tasks = [];
  const api = (command, args) => {
    calls.push({ command, args });
    const r = deferred();
    requests.push(r);
    return r.promise;
  };
  const request = {
    source: "CurseForge",
    project_id: "123",
    version_id: "999",
    file_name: "example.jar",
  };
  const ui = communityHarness("ResourceSave", {
    api,
    request,
    contextKey: "synthetic-save",
    native: true,
    disabled: false,
    onTaskStart(id) {
      tasks.push(id);
    },
    onClose() {},
    ...extra,
  });
  return { ui, calls, requests, tasks };
}
function preview(request, extra = {}) {
  return {
    token: "preview-token",
    preferencesRevision: "prefs-1",
    target: "/synthetic/example.jar",
    plan: {
      request,
      project_title: "Example",
      version_name: "Official file",
      file_name: "example.jar",
      size: 100,
      sha512: request.source === "CurseForge" ? "" : "a".repeat(128),
      sha1: "b".repeat(40),
      revision: "plan-1",
      ...extra,
    },
  };
}

test("CurseForge save preserves source through preparation and verifies SHA1 before token-only start", async () => {
  const { ui, calls, requests, tasks } = saver();
  assert.equal(calls[0].args.request.source, "CurseForge");
  requests[0].resolve(preview(ui.props.request));
  await ui.flush();
  assert.ok(content(ui.tree).includes(`SHA1: ${"b".repeat(40)}`));
  ui.tree.props.onConfirm();
  await ui.flush();
  assert.deepEqual(calls[1], {
    command: "resource_save_start",
    args: { token: "preview-token" },
  });
  requests[1].resolve({ id: "task-cf" });
  await ui.flush();
  assert.deepEqual(tasks, ["task-cf"]);
  ui.close();
});

test("save rejects previews from another provider despite colliding IDs", async () => {
  const { ui, calls, requests } = saver();
  requests[0].resolve(preview({ ...ui.props.request, source: "Modrinth" }));
  await ui.flush();
  assert.ok(content(ui.tree).includes(ui.i18n.t("save.selectionChanged")));
  assert.equal(calls.length, 1);
  ui.close();
});

test("CurseForge save rejects missing official hash", async () => {
  const { ui, requests } = saver();
  requests[0].resolve(preview(ui.props.request, { sha1: null }));
  await ui.flush();
  assert.ok(content(ui.tree).includes(ui.i18n.t("save.selectionChanged")));
  ui.close();
});

test("save provider changes retire a prepared confirmation with the same IDs", async () => {
  const { ui, calls, requests } = saver();
  requests[0].resolve(preview(ui.props.request));
  await ui.flush();
  const oldConfirm = ui.tree.props.onConfirm;
  ui.replace({ request: { ...ui.props.request, source: "Modrinth" } });
  oldConfirm();
  await ui.flush();
  assert.equal(
    calls.filter((call) => call.command === "resource_save_start").length,
    0,
  );
  assert.equal(calls.length, 2);
  ui.close();
});

test("legacy Modrinth saves accept omitted provider and continue showing SHA512", async () => {
  const request = {
    project_id: "Abcd1234",
    version_id: "Version1",
    file_name: "example.jar",
  };
  const { ui, calls, requests } = saver({ request });
  assert.equal(Object.hasOwn(calls[0].args.request, "source"), false);
  requests[0].resolve(preview(request));
  await ui.flush();
  assert.ok(content(ui.tree).includes(`SHA512: ${"a".repeat(128)}`));
  ui.close();
});

test("legacy Modrinth installer rejects CurseForge IDs before native invocation", () => {
  const { ui } = saver();
  const { resourceInstallRequest } = ui.module("resourceInstallPlan");
  assert.throws(() => resourceInstallRequest(ui.props.request));
  assert.deepEqual(
    resourceInstallRequest({
      project_id: "MR1",
      version_id: "MRV1",
      file_name: null,
    }),
    { project_id: "MR1", version_id: "MRV1", file_name: null },
  );
  ui.close();
});

function communityHelpers() {
  const { ui } = catalog();
  const helpers = ui.module("communityResources");
  ui.close();
  return helpers;
}

test("encyclopedia icons match the native image host allowlist", () => {
  const { communityIcon } = communityHelpers();
  for (const host of ["i.mcmod.cn", "www.mcmod.cn"])
    assert.equal(
      communityIcon(`https://${host}/cover/example.png`),
      `https://${host}/cover/example.png`,
    );
  for (const url of [
    "https://static.mcmod.cn/example.png",
    "https://i.mcmod.cn.evil.invalid/example.png",
    "https://another.mcmod.cn/example.png",
    "https://user@i.mcmod.cn/example.png",
    "https://i.mcmod.cn:8443/example.png",
    "http://i.mcmod.cn/example.png",
    "data:image/png;base64,AAAA",
    "file:///example.png",
  ])
    assert.equal(communityIcon(url), undefined, url);
});

test("catalog renders the encyclopedia image URL accepted by the backend", async () => {
  const { ui, requests } = catalog();
  const url = "https://i.mcmod.cn/modpack/cover/example.png";
  requests[0].resolve({
    hits: [summary({ source: "MC百科", icon_url: url })],
    warnings: [],
  });
  await ui.flush();
  assert.equal(
    elements(
      ui.tree,
      (node) => node.type === "img" && node.props.className === "ce-mod-icon",
    )[0].props.src,
    url,
  );
  ui.close();
});

test("URL-only aliases upgrade to a resolved ID once in either arrival order", () => {
  const { communityAliases } = communityHelpers();
  const resource = { source: "local", project_id: "" };
  for (const identified of aliases.slice(0, 2)) {
    const link = { ...identified, project_id: null };
    assert.deepEqual(communityAliases(resource, [link], [identified]), [
      identified,
    ]);
    assert.deepEqual(communityAliases(resource, [identified], [link]), [
      identified,
    ]);
  }
});

test("project URL normalization joins canonical and public host variants", () => {
  const { communityAliases } = communityHelpers();
  const resource = { source: "local", project_id: "" };
  const identified = aliases[1];
  const link = {
    ...identified,
    project_id: null,
    url: "http://www.modrinth.com/mod/Abcd1234/?ref=search#about",
  };
  assert.deepEqual(communityAliases(resource, [link], [identified]), [
    identified,
  ]);
  const encyclopedia = aliases[2];
  assert.deepEqual(
    communityAliases(
      resource,
      [
        {
          ...encyclopedia,
          project_id: null,
          url: "https://m.mcmod.cn/class/456.html/",
        },
      ],
      [encyclopedia],
    ),
    [encyclopedia],
  );
});

test("same provider ID with multiple URLs retains one action and resolves links to either URL", () => {
  const { communityAliases } = communityHelpers();
  const identified = aliases[1];
  const alternate = {
    ...identified,
    url: "https://modrinth.com/mod/example-slug",
  };
  assert.deepEqual(
    communityAliases(
      { ...identified, sources: [{ ...alternate, project_id: null }] },
      [alternate],
    ),
    [identified],
  );
});

test("a shared URL cannot merge different real provider IDs", () => {
  const { communityAliases } = communityHelpers();
  const first = aliases[1];
  const second = { ...first, project_id: "DifferentID" };
  assert.deepEqual(
    communityAliases({ source: "local", project_id: "" }, [
      { ...first, project_id: null },
      first,
      second,
    ]),
    [first, second],
  );
});

test("alias resolution keeps source namespaces and rejects unrelated URL hosts", () => {
  const { communityAliases } = communityHelpers();
  const first = aliases[0];
  const otherSource = { ...aliases[1], project_id: first.project_id };
  const unrelated = {
    ...first,
    project_id: null,
    url: "https://unrelated.invalid/minecraft/mc-mods/example",
  };
  assert.deepEqual(
    communityAliases({ source: "local", project_id: "" }, [
      first,
      otherSource,
      unrelated,
    ]),
    [first, otherSource, unrelated],
  );
});

test("detail resolution upgrades an encyclopedia search alias into one selectable source", async () => {
  const resource = summary({
    source: "MC百科",
    project_id: "456",
    url: aliases[2].url,
    sources: [aliases[2], { ...aliases[1], project_id: null }],
  });
  const { ui, requests, opened } = panel({ resource });
  requests[0].resolve(
    details({
      source: "MC百科",
      project_id: "456",
      versions: [],
      sources: [aliases[2], aliases[1]],
    }),
  );
  await ui.flush();
  const entries = elements(
    ui.tree,
    (node) =>
      node.type === "button" &&
      node.props.title ===
        ui.i18n.t("community.sourceSwitch", { source: "Modrinth" }),
  );
  assert.equal(entries.length, 1);
  assert.equal(
    elements(
      ui.tree,
      (node) =>
        node.type === "button" &&
        node.props.title ===
          ui.i18n.t("community.sourceOpen", { source: "Modrinth" }),
    ).length,
    0,
  );
  entries[0].props.onClick();
  assert.equal(opened[0].project_id, aliases[1].project_id);
  assert.equal(
    opened[0].sources.filter((alias) => alias.source === "Modrinth").length,
    1,
  );
  assert.equal(
    child(ui, "ResourceFavorite")[0].props.projectId,
    aliases[1].project_id,
  );
  ui.close();
});

test("fresh encyclopedia evidence retires a former provider source and its favorite", async () => {
  const { ui, requests } = panel({
    resource: summary({
      source: "MC百科",
      project_id: "456",
      url: aliases[2].url,
      sources: [aliases[2], aliases[1]],
    }),
  });
  assert.ok(selectSource(ui));
  assert.equal(
    child(ui, "ResourceFavorite")[0].props.projectId,
    aliases[1].project_id,
  );
  requests[0].resolve(
    details({
      source: "MC百科",
      project_id: "456",
      versions: [],
      sources: [aliases[2]],
    }),
  );
  await ui.flush();
  assert.equal(selectSource(ui), undefined);
  assert.equal(child(ui, "ResourceFavorite").length, 0);
  assert.equal(
    elements(
      ui.tree,
      (node) =>
        node.type === "button" &&
        node.props.title ===
          ui.i18n.t("community.sourceOpen", { source: "Modrinth" }),
    ).length,
    0,
  );
  ui.close();
});

test("fresh encyclopedia links replace a former URL even when the provider ID stays the same", async () => {
  const fresh = {
    ...aliases[1],
    url: "https://modrinth.com/mod/current-project",
  };
  const { ui, requests, opened } = panel({
    resource: summary({
      source: "MC百科",
      project_id: "456",
      url: aliases[2].url,
      sources: [aliases[2], aliases[1]],
    }),
  });
  requests[0].resolve(
    details({
      source: "MC百科",
      project_id: "456",
      versions: [],
      sources: [aliases[2], fresh],
    }),
  );
  await ui.flush();
  selectSource(ui).props.onClick();
  assert.equal(opened[0].url, fresh.url);
  assert.equal(
    opened[0].sources.filter((alias) => alias.source === "Modrinth").length,
    1,
  );
  assert.ok(!opened[0].sources.some((alias) => alias.url === aliases[1].url));
  ui.close();
});

test("encyclopedia project sources also supply the authoritative complete list", async () => {
  const { ui, requests } = panel({
    resource: summary({
      source: "MC百科",
      project_id: "456",
      url: aliases[2].url,
      sources: [aliases[2], aliases[1]],
    }),
  });
  const fresh = details({ source: "MC百科", project_id: "456", versions: [] });
  fresh.project.sources = [aliases[2]];
  requests[0].resolve(fresh);
  await ui.flush();
  assert.equal(selectSource(ui), undefined);
  assert.equal(child(ui, "ResourceFavorite").length, 0);
  ui.close();
});

test("ordinary provider details keep verified search links beyond their own primary source", async () => {
  const { ui, requests } = panel({ resource: summary({ sources: aliases }) });
  requests[0].resolve(details({ sources: [aliases[0]] }));
  await ui.flush();
  assert.ok(selectSource(ui));
  assert.equal(
    child(ui, "ResourceFavorite")[0].props.projectId,
    aliases[1].project_id,
  );
  ui.close();
});

test("a retired encyclopedia read cannot replace the current root's complete source list", async () => {
  const fresh = {
    ...aliases[1],
    url: "https://modrinth.com/mod/current-root-project",
  };
  const { ui, requests, opened } = panel({
    resource: summary({
      source: "MC百科",
      project_id: "456",
      url: aliases[2].url,
      sources: [aliases[2], aliases[1]],
    }),
  });
  ui.replace({ scopeKey: "root-b" });
  requests[1].resolve(
    details({
      source: "MC百科",
      project_id: "456",
      versions: [],
      sources: [aliases[2], fresh],
    }),
  );
  await ui.flush();
  requests[0].resolve(
    details({
      source: "MC百科",
      project_id: "456",
      versions: [],
      sources: [aliases[2]],
    }),
  );
  await ui.flush();
  selectSource(ui).props.onClick();
  assert.equal(opened[0].url, fresh.url);
  assert.equal(
    child(ui, "ResourceFavorite")[0].props.projectId,
    fresh.project_id,
  );
  ui.close();
});
