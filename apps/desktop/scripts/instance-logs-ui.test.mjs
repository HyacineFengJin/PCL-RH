/** Real instance-log hooks and handlers, with synthetic list/read/export replies.
 * No native application, game, account or actual game-root/log access is used. */
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  harness,
  elements,
  content,
  button,
  deferred,
} from "./maker-test-harness.mjs";

function listing(extra = {}) {
  return {
    id: "sample",
    game_dir: "/synthetic/shared-game",
    shared: true,
    revision: "listing-a",
    entries: [
      {
        kind: "game",
        name: "latest.log",
        size: 123,
        modified: 0,
        revision: "latest-a",
      },
      {
        kind: "game",
        name: "archive.log.gz",
        size: 1024,
        modified: null,
        revision: "archive-a",
      },
      {
        kind: "crash",
        name: "crash-report.txt",
        size: 1024 * 1024,
        modified: 1_700_000_000,
        revision: "crash-a",
      },
    ],
    truncated: false,
    ...extra,
  };
}
function fileContent(extra = {}) {
  return {
    id: "sample",
    kind: "game",
    name: "latest.log",
    text: "Synthetic current log\nSecond line",
    truncated: false,
    content_revision: "content-a",
    ...extra,
  };
}
function dialog(extra = {}) {
  const calls = [],
    requests = [];
  let closes = 0;
  const ui = harness("InstanceLogs", {
    id: "sample",
    scopeKey: "synthetic-root-a:instances:1",
    revision: "settings-a",
    api: async (command, args) => {
      calls.push({ command, args });
      const request = deferred();
      requests.push(request);
      return request.promise;
    },
    native: true,
    disabled: false,
    language: "zh-CN",
    region: "zh-CN",
    onClose() {
      closes++;
    },
    ...extra,
  });
  return {
    ui,
    calls,
    requests,
    get closes() {
      return closes;
    },
  };
}
function modal(ui) {
  return elements(ui.tree, (node) => node.props?.role === "dialog")[0];
}
function file(ui, name = "latest.log", kind = "game") {
  const group = elements(
    ui.tree,
    (node) => node.type === "section" && node.key === kind,
  )[0];
  const value = elements(
    group,
    (node) =>
      node.type === "button" &&
      node.props.className === "ce-instance-logs-file" &&
      content(node.props.children[0]) === name,
  )[0];
  assert.ok(value, `Missing ${kind} file ${name}`);
  return value;
}
function logContent(ui) {
  return elements(
    ui.tree,
    (node) => node.type?.name === "LauncherLogContent",
  )[0];
}
function exportButton(ui) {
  return elements(
    ui.tree,
    (node) =>
      node.type === "button" &&
      node.props.className === "ce-button ce-instance-logs-export",
  )[0];
}
async function listed(extra = {}, response = listing()) {
  const fixture = dialog(extra);
  fixture.requests[0].resolve(response);
  await fixture.ui.flush();
  return fixture;
}
async function loaded(response = fileContent(), extra = {}) {
  const fixture = await listed(extra);
  file(fixture.ui).props.onClick();
  fixture.requests[1].resolve(response);
  await fixture.ui.flush();
  return fixture;
}

function exported(extra = {}) {
  return {
    status: "complete",
    path: "/synthetic/chosen/log.txt",
    count: 1,
    warning: null,
    ...extra,
  };
}

test("mount lists once and never reads or selects the latest file automatically", async () => {
  const { ui, calls, requests } = dialog();
  assert.deepEqual(calls, [
    { command: "instance_logs", args: { id: "sample" } },
  ]);
  assert.equal(modal(ui).props["aria-busy"], true);
  const refresh = button(ui, "instanceLogs.refresh");
  assert.equal(refresh.props.disabled, true);
  refresh.props.onClick();
  refresh.props.onClick();
  ui.replace({});
  await ui.flush();
  assert.equal(calls.length, 1);
  requests[0].resolve(listing());
  await ui.flush();
  assert.equal(calls.length, 1);
  assert.equal(modal(ui).props["aria-busy"], false);
  assert.equal(file(ui).props["aria-pressed"], false);
  assert.ok(content(ui.tree).includes(ui.i18n.t("instanceLogs.selectFile")));
  assert.equal(logContent(ui), undefined);
  ui.close();
});

test("files show their kind, regional time, on-disk size and shared-directory identity", async () => {
  const { ui, calls } = await listed();
  const text = content(ui.tree);
  assert.ok(text.includes("游戏日志 · sample"));
  assert.ok(text.includes(ui.i18n.t("instanceLogs.game")));
  assert.ok(text.includes(ui.i18n.t("instanceLogs.crash")));
  assert.ok(text.includes(ui.i18n.t("instanceLogs.shared")));
  assert.ok(text.includes("/synthetic/shared-game"));
  assert.ok(content(file(ui)).includes("123 B"));
  assert.ok(content(file(ui, "archive.log.gz")).includes("1 KiB"));
  assert.ok(content(file(ui, "archive.log.gz")).includes("—"));
  assert.ok(content(file(ui, "crash-report.txt", "crash")).includes("1 MiB"));
  assert.ok(
    content(file(ui)).includes(
      ui.i18n.formatDate(0, { dateStyle: "short", timeStyle: "short" }),
    ),
  );
  assert.equal(calls.length, 1);
  ui.close();
});

test("unshared and empty directories have a clear empty state without a file read", async () => {
  const { ui, calls } = await listed(
    {},
    listing({ shared: false, entries: [] }),
  );
  assert.ok(content(ui.tree).includes(ui.i18n.t("instanceLogs.empty")));
  assert.ok(!content(ui.tree).includes(ui.i18n.t("instanceLogs.shared")));
  assert.equal(logContent(ui), undefined);
  assert.equal(calls.length, 1);
  assert.equal(button(ui, "instanceLogs.refresh").props.disabled, false);
  ui.close();
});

test("an explicit selection binds the exact file and listing revisions", async () => {
  const { ui, calls, requests } = await listed();
  file(ui).props.onClick();
  await ui.flush();
  assert.deepEqual(calls[1], {
    command: "instance_read_log",
    args: {
      id: "sample",
      kind: "game",
      name: "latest.log",
      revision: "latest-a",
      listingRevision: "listing-a",
    },
  });
  assert.equal(file(ui).props["aria-pressed"], true);
  assert.equal(modal(ui).props["aria-busy"], true);
  assert.equal(logContent(ui), undefined);
  requests[1].resolve(fileContent());
  await ui.flush();
  assert.equal(logContent(ui).props.text, fileContent().text);
  assert.equal(logContent(ui).props.language, "zh-CN");
  assert.equal(logContent(ui).props.region, "zh-CN");
  assert.equal(modal(ui).props["aria-busy"], false);
  ui.close();
});

test("gzip rows display native-decoded text without an additional read or decoder", async () => {
  const { ui, calls, requests } = await listed();
  file(ui, "archive.log.gz").props.onClick();
  requests[1].resolve(
    fileContent({ name: "archive.log.gz", text: "Synthetic decoded archive" }),
  );
  await ui.flush();
  assert.equal(calls.length, 2);
  assert.equal(logContent(ui).props.text, "Synthetic decoded archive");
  ui.close();
});

test("identical names in game and crash groups preserve the selected kind", async () => {
  const response = listing();
  response.entries[2].name = "latest.log";
  const { ui, calls, requests } = await listed({}, response);
  file(ui, "latest.log", "crash").props.onClick();
  requests[1].resolve(
    fileContent({ kind: "crash", text: "Crash-only content" }),
  );
  await ui.flush();
  assert.equal(calls[1].args.kind, "crash");
  assert.equal(calls[1].args.revision, "crash-a");
  assert.equal(logContent(ui).props.text, "Crash-only content");
  assert.equal(file(ui).props["aria-pressed"], false);
  assert.equal(file(ui, "latest.log", "crash").props["aria-pressed"], true);
  ui.close();
});

test("selecting another file retires an earlier late success and failure", async () => {
  for (const lateFailure of [false, true]) {
    const { ui, requests } = await listed();
    file(ui).props.onClick();
    file(ui, "crash-report.txt", "crash").props.onClick();
    requests[2].resolve(
      fileContent({
        kind: "crash",
        name: "crash-report.txt",
        text: "Current crash",
      }),
    );
    await ui.flush();
    if (lateFailure) requests[1].reject(new Error("Retired file failure"));
    else requests[1].resolve(fileContent({ text: "Retired game log" }));
    await ui.flush();
    assert.equal(logContent(ui).props.text, "Current crash");
    assert.ok(!content(ui.tree).includes("Retired file failure"));
    ui.close();
  }
});

test("clicking the same row rereads and remounts search even for identical text", async () => {
  const { ui, calls, requests } = await listed();
  const reread = file(ui).props.onClick;
  reread();
  requests[1].resolve(fileContent());
  await ui.flush();
  const firstKey = logContent(ui).key;
  reread();
  await ui.flush();
  assert.equal(logContent(ui), undefined);
  requests[2].resolve(fileContent());
  await ui.flush();
  assert.equal(calls.length, 3);
  assert.notEqual(logContent(ui).key, firstKey);
  assert.equal(logContent(ui).props.text, fileContent().text);
  ui.close();
});

test("an earlier read of the same row cannot overwrite its later reread", async () => {
  const { ui, requests } = await listed();
  const read = file(ui).props.onClick;
  read();
  read();
  requests[2].resolve(fileContent({ text: "Current reread" }));
  await ui.flush();
  requests[1].resolve(fileContent({ text: "Earlier read" }));
  await ui.flush();
  assert.equal(logContent(ui).props.text, "Current reread");
  ui.close();
});

test("refresh immediately retires saved rows, selection, text and the pending read", async () => {
  const { ui, calls, requests } = await listed();
  const oldRow = file(ui).props.onClick;
  oldRow();
  requests[1].resolve(fileContent());
  await ui.flush();
  oldRow();
  button(ui, "instanceLogs.refresh").props.onClick();
  oldRow();
  assert.equal(calls.length, 4);
  await ui.flush();
  assert.equal(logContent(ui), undefined);
  requests[2].resolve(fileContent({ text: "Read retired by refresh" }));
  requests[3].resolve(listing({ revision: "listing-b" }));
  await new Promise((done) => setImmediate(done));
  oldRow();
  assert.equal(calls.length, 4);
  await ui.flush();
  oldRow();
  assert.equal(calls.length, 4);
  assert.equal(file(ui).props["aria-pressed"], false);
  assert.equal(logContent(ui), undefined);
  file(ui).props.onClick();
  assert.equal(calls[4].args.listingRevision, "listing-b");
  ui.close();
});

test("file disappearance and unreadable files show the selected failure without other-file fallback", async () => {
  for (const reason of [
    "Synthetic file missing",
    "Synthetic file permission denied",
  ]) {
    const { ui, requests, calls } = await listed();
    file(ui).props.onClick();
    requests[1].resolve(fileContent({ text: "Previous successful content" }));
    await ui.flush();
    file(ui, "crash-report.txt", "crash").props.onClick();
    requests[2].reject(new Error(reason));
    await ui.flush();
    assert.equal(logContent(ui), undefined);
    assert.ok(content(ui.tree).includes(reason));
    assert.ok(content(ui.tree).includes(ui.i18n.t("instanceLogs.readFailed")));
    assert.ok(!content(ui.tree).includes("Previous successful content"));
    assert.equal(calls.length, 3);
    assert.equal(
      file(ui, "crash-report.txt", "crash").props["aria-pressed"],
      true,
    );
    assert.equal(button(ui, "instanceLogs.refresh").props.disabled, false);
    ui.close();
  }
});

test("read replies must match all of instance, kind and filename", async () => {
  for (const wrong of [
    { id: "other" },
    { kind: "crash" },
    { name: "other.log" },
  ]) {
    const { ui, requests, calls } = await listed();
    file(ui).props.onClick();
    requests[1].resolve(fileContent({ text: "Wrong-file secret", ...wrong }));
    await ui.flush();
    assert.equal(logContent(ui), undefined);
    assert.ok(content(ui.tree).includes(ui.i18n.t("instanceLogs.wrongFile")));
    assert.ok(!content(ui.tree).includes("Wrong-file secret"));
    assert.equal(calls.length, 2);
    ui.close();
  }
});

test("list errors and wrong-instance replies remain retryable and expose no stale rows", async () => {
  const { ui, requests, calls } = await listed();
  button(ui, "instanceLogs.refresh").props.onClick();
  requests[1].reject(new Error("Synthetic listing failure"));
  await ui.flush();
  assert.ok(content(ui.tree).includes("Synthetic listing failure"));
  assert.ok(!content(ui.tree).includes("latest.log"));
  button(ui, "instanceLogs.refresh").props.onClick();
  requests[2].resolve(
    listing({ id: "other", game_dir: "/retired/private-path" }),
  );
  await ui.flush();
  assert.ok(content(ui.tree).includes(ui.i18n.t("instanceLogs.wrongInstance")));
  assert.ok(!content(ui.tree).includes("/retired/private-path"));
  button(ui, "instanceLogs.refresh").props.onClick();
  requests[3].resolve(listing());
  await ui.flush();
  assert.equal(calls.length, 4);
  file(ui);
  assert.equal(
    elements(ui.tree, (node) => node.props?.role === "alert").length,
    0,
  );
  ui.close();
});

test("list and content truncation are independent, short notices and empty files retain search", async () => {
  const { ui, requests } = await listed({}, listing({ truncated: true }));
  assert.ok(content(ui.tree).includes(ui.i18n.t("instanceLogs.listTruncated")));
  assert.ok(
    !content(ui.tree).includes(ui.i18n.t("instanceLogs.textTruncated")),
  );
  file(ui).props.onClick();
  requests[1].resolve(fileContent({ truncated: true }));
  await ui.flush();
  assert.ok(content(ui.tree).includes(ui.i18n.t("instanceLogs.textTruncated")));
  assert.equal(logContent(ui).props.text, fileContent().text);
  file(ui).props.onClick();
  requests[2].resolve(fileContent({ text: "" }));
  await ui.flush();
  assert.ok(content(ui.tree).includes(ui.i18n.t("instanceLogs.emptyFile")));
  assert.ok(
    !content(ui.tree).includes(ui.i18n.t("instanceLogs.textTruncated")),
  );
  assert.equal(logContent(ui).props.text, "");
  ui.close();
});

test("id, navigation/root scope and settings revisions retire saved handlers and pending listings", async () => {
  for (const change of [
    { id: "other-instance" },
    { scopeKey: "synthetic-root-b:instances:2" },
    { revision: "settings-b" },
  ]) {
    const fixture = dialog(),
      { ui, calls, requests } = fixture;
    const oldClose = button(ui, "instanceLogs.close").props.onClick;
    const oldRefresh = button(ui, "instanceLogs.refresh").props.onClick;
    ui.replace(change);
    oldClose();
    oldRefresh();
    requests[0].resolve(listing({ game_dir: "/retired/root" }));
    await ui.flush();
    assert.equal(calls.length, 2);
    assert.equal(fixture.closes, 0);
    assert.equal(logContent(ui), undefined);
    assert.ok(!content(ui.tree).includes("/retired/root"));
    requests[1].resolve(listing({ id: change.id || "sample" }));
    await ui.flush();
    file(ui);
    ui.close();
  }
});

test("scope changes clear selected content immediately and permanently retire its row and read", async () => {
  for (const change of [
    { id: "other-instance" },
    { scopeKey: "synthetic-root-b" },
    { revision: "settings-b" },
  ]) {
    const { ui, requests, calls } = await listed();
    const oldRow = file(ui).props.onClick;
    oldRow();
    requests[1].resolve(fileContent());
    await ui.flush();
    oldRow();
    ui.replace(change);
    oldRow();
    assert.equal(logContent(ui), undefined);
    requests[2].resolve(fileContent({ text: "Retired scope content" }));
    requests[3].resolve(listing({ id: change.id || "sample" }));
    await ui.flush();
    oldRow();
    assert.equal(calls.length, 4);
    assert.equal(logContent(ui), undefined);
    assert.equal(file(ui).props["aria-pressed"], false);
    ui.close();
  }
});

test("API replacement rejects both the old list response and saved file handlers", async () => {
  const { ui, requests, calls } = await listed();
  const oldRow = file(ui).props.onClick;
  button(ui, "instanceLogs.refresh").props.onClick();
  const replacement = deferred();
  let replacementCalls = 0;
  ui.replace({
    api: async (command, args) => {
      replacementCalls++;
      assert.equal(command, "instance_logs");
      assert.deepEqual(args, { id: "sample" });
      return replacement.promise;
    },
  });
  oldRow();
  replacement.resolve(listing({ game_dir: "/current/API" }));
  await ui.flush();
  requests[1].resolve(listing({ game_dir: "/retired/API" }));
  await ui.flush();
  oldRow();
  assert.equal(calls.length, 2);
  assert.equal(replacementCalls, 1);
  assert.ok(content(ui.tree).includes("/current/API"));
  assert.ok(!content(ui.tree).includes("/retired/API"));
  ui.close();
});

test("initial disabled and preview scopes do not read; enabling starts a fresh listing", async () => {
  for (const extra of [{ native: false }, { disabled: true }]) {
    const { ui, calls, requests } = dialog(extra);
    assert.equal(calls.length, 0);
    assert.equal(button(ui, "instanceLogs.refresh").props.disabled, true);
    button(ui, "instanceLogs.refresh").props.onClick();
    assert.equal(calls.length, 0);
    assert.ok(
      content(ui.tree).includes(
        ui.i18n.t(
          extra.native === false
            ? "instanceLogs.desktopOnly"
            : "instanceLogs.disabled",
        ),
      ),
    );
    ui.replace({ native: true, disabled: false });
    assert.equal(calls.length, 1);
    requests[0].resolve(listing());
    await ui.flush();
    file(ui);
    ui.close();
  }
});

test("disabling then enabling permanently retires pending reads and every saved scope handler", async () => {
  for (const flag of ["disabled", "native"]) {
    const fixture = await listed(),
      { ui, requests, calls } = fixture;
    const oldRow = file(ui).props.onClick;
    const oldRefresh = button(ui, "instanceLogs.refresh").props.onClick;
    const oldClose = button(ui, "instanceLogs.close").props.onClick;
    oldRow();
    ui.replace({ [flag]: flag === "disabled" });
    oldRow();
    oldRefresh();
    oldClose();
    assert.equal(logContent(ui), undefined);
    ui.replace({ [flag]: flag === "native" });
    oldRow();
    oldRefresh();
    oldClose();
    requests[1].reject(new Error("Retired disabled read"));
    requests[2].resolve(listing());
    await ui.flush();
    oldRow();
    oldRefresh();
    oldClose();
    assert.equal(calls.length, 3);
    assert.equal(fixture.closes, 0);
    assert.equal(logContent(ui), undefined);
    assert.ok(!content(ui.tree).includes("Retired disabled read"));
    file(ui);
    ui.close();
  }
});

test("close and unmount retire pending replies and saved row, refresh and close handlers", async () => {
  for (const mode of ["close", "unmount"]) {
    const fixture = await listed(),
      { ui, requests, calls } = fixture;
    const read = file(ui).props.onClick;
    const close = button(ui, "instanceLogs.close").props.onClick;
    const refresh = button(ui, "instanceLogs.refresh").props.onClick;
    read();
    if (mode === "unmount") ui.close();
    close();
    close();
    refresh();
    read();
    requests[1].resolve(fileContent({ text: "Retired closed content" }));
    await ui.flush();
    assert.equal(calls.length, 2);
    assert.equal(fixture.closes, mode === "close" ? 1 : 0);
    assert.equal(logContent(ui), undefined);
    ui.close();
  }
});

test("Escape and backdrop close retire pending lists while inner clicks keep the dialog", async () => {
  for (const mode of ["escape", "backdrop"]) {
    const fixture = dialog(),
      { ui, requests } = fixture;
    const shade = elements(
      ui.tree,
      (node) => node.props?.className === "modal-shade ce-instance-logs-shade",
    )[0];
    shade.props.onMouseDown({ target: {}, currentTarget: shade });
    assert.equal(fixture.closes, 0);
    if (mode === "escape")
      modal(ui).props.onKeyDown({
        key: "Escape",
        preventDefault() {},
        stopPropagation() {},
      });
    else shade.props.onMouseDown({ target: shade, currentTarget: shade });
    requests[0].resolve(listing());
    await ui.flush();
    assert.equal(fixture.closes, 1);
    assert.ok(!content(ui.tree).includes("latest.log"));
    ui.close();
  }
});

test("Tab captures outside focus, and an empty control set safely focuses the dialog", async () => {
  const { ui } = dialog();
  let focused = "",
    prevented = 0;
  const controls = [
    {
      focus() {
        focused = "first";
      },
    },
    {
      focus() {
        focused = "last";
      },
    },
  ];
  for (const shiftKey of [false, true]) {
    modal(ui).props.onKeyDown({
      key: "Tab",
      shiftKey,
      currentTarget: {
        querySelectorAll() {
          return controls;
        },
      },
      preventDefault() {
        prevented++;
      },
      stopPropagation() {},
    });
    assert.equal(focused, shiftKey ? "last" : "first");
  }
  modal(ui).props.onKeyDown({
    key: "Tab",
    currentTarget: {
      querySelectorAll() {
        return [];
      },
      focus() {
        focused = "dialog";
      },
    },
    preventDefault() {
      prevented++;
    },
    stopPropagation() {},
  });
  assert.equal(focused, "dialog");
  assert.equal(prevented, 3);
  assert.equal(modal(ui).props.tabIndex, -1);
  ui.close();
});

test("language and region update presentation without rereading or resetting selected content", async () => {
  const { ui, requests, calls } = await listed();
  file(ui).props.onClick();
  requests[1].resolve(fileContent());
  await ui.flush();
  const searchKey = logContent(ui).key;
  ui.replace({ language: "en-US", region: "en-US" });
  const english = ui.i18n.createTranslator({
    language: "en-US",
    region: "en-US",
  });
  assert.ok(content(ui.tree).includes("Game logs · sample"));
  assert.ok(content(ui.tree).includes(english.t("instanceLogs.shared")));
  assert.ok(
    content(file(ui)).includes(
      english.formatDate(0, { dateStyle: "short", timeStyle: "short" }),
    ),
  );
  assert.equal(logContent(ui).props.language, "en-US");
  assert.equal(logContent(ui).props.region, "en-US");
  assert.equal(logContent(ui).key, searchKey);
  assert.equal(calls.length, 2);
  ui.close();
});

test("all instance-log messages exist in Chinese and English", () => {
  const { ui } = dialog({ native: false });
  const chinese = ui.i18n.createTranslator({
    language: "zh-CN",
    region: "zh-CN",
  });
  const english = ui.i18n.createTranslator({
    language: "en-US",
    region: "en-US",
  });
  for (const suffix of [
    "open",
    "title",
    "close",
    "refresh",
    "export",
    "exporting",
    "exported",
    "exportFailed",
    "exportUnavailable",
    "desktopOnly",
    "disabled",
    "loading",
    "reading",
    "listFailed",
    "readFailed",
    "wrongInstance",
    "wrongFile",
    "shared",
    "game",
    "crash",
    "content",
    "empty",
    "selectFile",
    "emptyFile",
    "listTruncated",
    "textTruncated",
  ]) {
    const key = `instanceLogs.${suffix}`;
    assert.notEqual(chinese.t(key), key);
    assert.notEqual(english.t(key), key);
  }
  ui.close();
});

test("export requires a successfully loaded nonempty text and native content receipt", async () => {
  for (const response of [
    fileContent({ text: "" }),
    fileContent({ content_revision: undefined }),
    fileContent({ content_revision: "" }),
  ]) {
    const { ui, calls, requests } = await listed();
    assert.equal(exportButton(ui), undefined);
    file(ui).props.onClick();
    await ui.flush();
    assert.equal(exportButton(ui).props.disabled, true);
    exportButton(ui).props.onClick();
    requests[1].resolve(response);
    await ui.flush();
    assert.equal(exportButton(ui).props.disabled, true);
    exportButton(ui).props.onClick();
    assert.equal(calls.length, 2);
    assert.equal(logContent(ui).props.text, response.text);
    ui.close();
  }
  const { ui, calls, requests } = await listed();
  file(ui).props.onClick();
  requests[1].reject(new Error("Synthetic unreadable file"));
  await ui.flush();
  assert.equal(exportButton(ui).props.disabled, true);
  exportButton(ui).props.onClick();
  assert.equal(calls.length, 2);
  ui.close();
});

test("export binds all native receipts and never resends text or rereads a gzip file", async () => {
  const { ui, calls, requests } = await listed();
  file(ui, "archive.log.gz").props.onClick();
  requests[1].resolve(
    fileContent({
      name: "archive.log.gz",
      text: "Synthetic decoded, redacted tail",
      content_revision: "gzip-content-receipt",
      truncated: true,
    }),
  );
  await ui.flush();
  assert.equal(exportButton(ui).props.disabled, false);
  assert.ok(content(ui.tree).includes(ui.i18n.t("instanceLogs.textTruncated")));
  exportButton(ui).props.onClick();
  assert.deepEqual(calls[2], {
    command: "instance_export_log",
    args: {
      id: "sample",
      kind: "game",
      name: "archive.log.gz",
      revision: "archive-a",
      listingRevision: "listing-a",
      contentRevision: "gzip-content-receipt",
    },
  });
  requests[2].resolve(exported());
  await ui.flush();
  assert.equal(calls.length, 3);
  assert.equal(logContent(ui).props.text, "Synthetic decoded, redacted tail");
  ui.close();
});

test("completed export preserves content and search, with a compact path receipt and warning", async () => {
  const { ui, requests } = await loaded();
  const searchKey = logContent(ui).key;
  exportButton(ui).props.onClick();
  requests[2].resolve(exported({ warning: "Synthetic durability notice" }));
  await ui.flush();
  assert.ok(content(ui.tree).includes(ui.i18n.t("instanceLogs.exported")));
  assert.ok(content(ui.tree).includes("Synthetic durability notice"));
  assert.equal(
    elements(ui.tree, (node) => node.props?.title === exported().path).length,
    1,
  );
  assert.equal(logContent(ui).props.text, fileContent().text);
  assert.equal(logContent(ui).key, searchKey);
  assert.equal(exportButton(ui).props.disabled, false);
  assert.equal(modal(ui).props["aria-busy"], false);
  ui.close();
});

test("cancel is silent and retryable; unavailable and failed saves preserve the selected text", async () => {
  const { ui, calls, requests } = await loaded();
  exportButton(ui).props.onClick();
  requests[2].resolve(exported({ status: "cancelled", path: null, count: 0 }));
  await ui.flush();
  assert.ok(!content(ui.tree).includes(ui.i18n.t("instanceLogs.exported")));
  assert.ok(!content(ui.tree).includes(ui.i18n.t("instanceLogs.exportFailed")));
  assert.equal(exportButton(ui).props.disabled, false);
  exportButton(ui).props.onClick();
  requests[3].resolve(
    exported({
      status: "unavailable",
      path: null,
      count: 0,
      warning: "Synthetic unavailable reason",
    }),
  );
  await ui.flush();
  assert.ok(
    content(ui.tree).includes(ui.i18n.t("instanceLogs.exportUnavailable")),
  );
  assert.ok(content(ui.tree).includes("Synthetic unavailable reason"));
  exportButton(ui).props.onClick();
  requests[4].reject(new Error("Synthetic destination failure"));
  await ui.flush();
  assert.ok(content(ui.tree).includes(ui.i18n.t("instanceLogs.exportFailed")));
  assert.ok(content(ui.tree).includes("Synthetic destination failure"));
  assert.ok(!content(ui.tree).includes("Synthetic unavailable reason"));
  assert.equal(exportButton(ui).props.disabled, false);
  assert.equal(logContent(ui).props.text, fileContent().text);
  assert.equal(calls.length, 5);
  ui.close();
});

test("one busy claim prevents duplicate choosers while file reads and refresh remain available", async () => {
  const { ui, calls, requests } = await loaded();
  const save = exportButton(ui).props.onClick;
  save();
  save();
  await ui.flush();
  assert.equal(exportButton(ui).props.disabled, true);
  assert.ok(
    content(exportButton(ui)).includes(ui.i18n.t("instanceLogs.exporting")),
  );
  assert.equal(modal(ui).props["aria-busy"], true);
  exportButton(ui).props.onClick();
  assert.equal(calls.length, 3);
  assert.equal(button(ui, "instanceLogs.refresh").props.disabled, false);
  file(ui, "crash-report.txt", "crash").props.onClick();
  requests[3].resolve(
    fileContent({
      kind: "crash",
      name: "crash-report.txt",
      text: "Current crash",
    }),
  );
  await ui.flush();
  assert.equal(logContent(ui).props.text, "Current crash");
  assert.equal(exportButton(ui).props.disabled, true);
  exportButton(ui).props.onClick();
  assert.equal(calls.length, 4);
  requests[2].resolve(exported({ warning: "Retired completion warning" }));
  await ui.flush();
  assert.equal(exportButton(ui).props.disabled, false);
  assert.ok(!content(ui.tree).includes("Retired completion warning"));
  exportButton(ui).props.onClick();
  assert.equal(calls.length, 5);
  assert.equal(calls[4].args.kind, "crash");
  requests[4].resolve(exported({ status: "cancelled" }));
  await ui.flush();
  ui.close();
});

test("reread, another selection and refresh immediately retire saved export handlers and late results", async () => {
  for (const change of ["reread", "selection", "refresh"]) {
    for (const outcome of ["complete", "error"]) {
      const { ui, calls, requests } = await loaded();
      const oldSave = exportButton(ui).props.onClick;
      oldSave();
      if (change === "refresh")
        button(ui, "instanceLogs.refresh").props.onClick();
      else
        file(
          ui,
          change === "selection" ? "archive.log.gz" : "latest.log",
        ).props.onClick();
      oldSave();
      assert.equal(calls.length, 4);
      if (change === "refresh") {
        requests[3].resolve(listing({ revision: "listing-b" }));
        await ui.flush();
        file(ui).props.onClick();
        requests[4].resolve(fileContent({ text: "Current refreshed text" }));
      } else {
        requests[3].resolve(
          fileContent({
            name: change === "selection" ? "archive.log.gz" : "latest.log",
            text: "Current selected text",
            content_revision: "content-b",
          }),
        );
      }
      await ui.flush();
      const selectedText = logContent(ui).props.text;
      oldSave();
      assert.equal(exportButton(ui).props.disabled, true);
      const before = calls.length;
      exportButton(ui).props.onClick();
      assert.equal(calls.length, before);
      if (outcome === "error")
        requests[2].reject(new Error("Retired save failure"));
      else requests[2].resolve(exported({ warning: "Retired save warning" }));
      await ui.flush();
      oldSave();
      assert.equal(calls.length, before);
      assert.equal(exportButton(ui).props.disabled, false);
      assert.equal(logContent(ui).props.text, selectedText);
      assert.ok(!content(ui.tree).includes("Retired save"));
      assert.ok(!content(ui.tree).includes(ui.i18n.t("instanceLogs.exported")));
      ui.close();
    }
  }
});

test("published reread retires a previous export callback before React commits", async () => {
  const { ui, calls, requests } = await loaded();
  const oldSave = exportButton(ui).props.onClick;
  file(ui).props.onClick();
  requests[2].resolve(fileContent({ text: "Published new text" }));
  await new Promise((done) => setImmediate(done));
  oldSave();
  assert.equal(calls.length, 3);
  await ui.flush();
  assert.equal(logContent(ui).props.text, "Published new text");
  assert.equal(exportButton(ui).props.disabled, false);
  ui.close();
});

test("instance, root/navigation and settings changes retain export busy until the old request settles", async () => {
  for (const change of [
    { id: "other-instance" },
    { scopeKey: "synthetic-root-b:instances:2" },
    { revision: "settings-b" },
  ]) {
    for (const outcome of ["complete", "error"]) {
      const { ui, calls, requests } = await loaded();
      const oldSave = exportButton(ui).props.onClick;
      oldSave();
      ui.replace(change);
      oldSave();
      assert.equal(calls.length, 4);
      const id = change.id || "sample";
      requests[3].resolve(listing({ id }));
      await ui.flush();
      file(ui).props.onClick();
      requests[4].resolve(fileContent({ id, text: "Current scope text" }));
      await ui.flush();
      assert.equal(exportButton(ui).props.disabled, true);
      exportButton(ui).props.onClick();
      assert.equal(calls.length, 5);
      if (outcome === "error")
        requests[2].reject(new Error("Retired scope save failure"));
      else requests[2].resolve(exported({ warning: "Retired scope warning" }));
      await ui.flush();
      oldSave();
      assert.equal(calls.length, 5);
      assert.equal(exportButton(ui).props.disabled, false);
      assert.equal(logContent(ui).props.text, "Current scope text");
      assert.ok(!content(ui.tree).includes("Retired scope"));
      assert.ok(!content(ui.tree).includes(ui.i18n.t("instanceLogs.exported")));
      ui.close();
    }
  }
});

test("API replacement retires the old export callback and result without releasing its busy claim early", async () => {
  const { ui, calls, requests } = await loaded();
  const oldSave = exportButton(ui).props.onClick;
  oldSave();
  const replacements = [],
    replacementCalls = [];
  ui.replace({
    api: async (command, args) => {
      replacementCalls.push({ command, args });
      const request = deferred();
      replacements.push(request);
      return request.promise;
    },
  });
  oldSave();
  replacements[0].resolve(listing());
  await ui.flush();
  file(ui).props.onClick();
  replacements[1].resolve(fileContent({ text: "Current API text" }));
  await ui.flush();
  assert.equal(exportButton(ui).props.disabled, true);
  exportButton(ui).props.onClick();
  assert.equal(replacementCalls.length, 2);
  requests[2].reject(new Error("Retired API save failure"));
  await ui.flush();
  oldSave();
  assert.equal(calls.length, 3);
  assert.equal(exportButton(ui).props.disabled, false);
  assert.ok(!content(ui.tree).includes("Retired API save failure"));
  assert.equal(logContent(ui).props.text, "Current API text");
  exportButton(ui).props.onClick();
  assert.equal(replacementCalls[2].command, "instance_export_log");
  replacements[2].resolve(exported({ status: "cancelled" }));
  await ui.flush();
  ui.close();
});

test("disable and preview transitions permanently retire export handlers and notices", async () => {
  for (const flag of ["disabled", "native"]) {
    const { ui, calls, requests } = await loaded();
    const oldSave = exportButton(ui).props.onClick;
    oldSave();
    ui.replace({ [flag]: flag === "disabled" });
    oldSave();
    assert.equal(logContent(ui), undefined);
    ui.replace({ [flag]: flag === "native" });
    oldSave();
    requests[3].resolve(listing());
    await ui.flush();
    file(ui).props.onClick();
    requests[4].resolve(fileContent({ text: "Current enabled text" }));
    await ui.flush();
    assert.equal(exportButton(ui).props.disabled, true);
    requests[2].resolve(exported({ warning: "Retired disabled warning" }));
    await ui.flush();
    oldSave();
    assert.equal(calls.length, 5);
    assert.equal(exportButton(ui).props.disabled, false);
    assert.ok(!content(ui.tree).includes("Retired disabled warning"));
    assert.ok(!content(ui.tree).includes(ui.i18n.t("instanceLogs.exported")));
    ui.close();
  }
});

test("close and unmount retire pending export results and saved chooser callbacks", async () => {
  for (const mode of ["close", "unmount"]) {
    for (const outcome of ["complete", "error"]) {
      const fixture = await loaded(),
        { ui, calls, requests } = fixture;
      const oldSave = exportButton(ui).props.onClick;
      oldSave();
      if (mode === "close") button(ui, "instanceLogs.close").props.onClick();
      else ui.close();
      oldSave();
      if (outcome === "error")
        requests[2].reject(new Error("Retired closed save failure"));
      else requests[2].resolve(exported({ warning: "Retired closed warning" }));
      await ui.flush();
      oldSave();
      assert.equal(calls.length, 3);
      assert.equal(fixture.closes, mode === "close" ? 1 : 0);
      assert.ok(!content(ui.tree).includes("Retired closed"));
      assert.ok(!content(ui.tree).includes(ui.i18n.t("instanceLogs.exported")));
      ui.close();
    }
  }
});
