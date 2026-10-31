/** Private stdio entry owned by Tauri. Declarative plugin management and cards
 * need only the bounded extension service. AI/project services are imported on
 * their first operation; plugin JSON never receives either dispatcher or tools.
 * This entry owns the protocol, process shutdown and the single extension store. */
import * as fs from "node:fs/promises";
import { constants } from "node:fs";
import path from "node:path";
import readline from "node:readline";
import { ExtensionStore } from "./extensions/src/store.mjs";
import { inspectForeignManifest } from "./extensions/src/compatibility.mjs";
import {
  foreignPackageFormat,
  inspectForeignPackageFile,
} from "./extensions/src/foreign-package.mjs";

const root = path.resolve(process.argv[2]);
await fs.mkdir(root, { recursive: true, mode: 0o700 });
const extensions = await ExtensionStore.open({ file: path.join(root, "extensions.json") });
let closing = false, agent = null, agentLoading = null, resumePresetId = null;

// Retain one loading promise, including failure. Repeating a partially failed
// factory could create a second set of service owners inside this process.
async function ensureAgent() {
  agentLoading ??= import("./agent-host.mjs").then(async ({ createAgentHost }) => {
    const owner = await createAgentHost({ root, extensions });
    // Native wakeup supplies only a durable locator. Defer its validation to
    // the original preset service without importing that service for wakeup.
    if (resumePresetId !== null) {
      await owner.dispatch("idle_resume", { presetId: resumePresetId });
      resumePresetId = null;
    }
    return owner;
  });
  agent = await agentLoading;
  return agent;
}

async function boundedRead(file, max = 128000) {
  const handle = await fs.open(file, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = await handle.stat();
    if (!stat.isFile() || stat.size > max)
      throw new Error("Expected a bounded regular file");
    const bytes = await handle.readFile();
    if (bytes.length > max) throw new Error("File exceeds limit");
    return bytes.toString("utf8");
  } finally {
    await handle.close();
  }
}
async function dispatch(operation, a) {
  if (closing && operation !== "shutdown")
    throw new Error("Experimental host is closing");
  switch (operation) {
    case "extensions_list":
      return { entries: extensions.list(), safeMode: extensions.safeMode, warning: extensions.warning };
    case "extensions_review": {
      // Foreign metadata remains inspection-only, including when the native
      // store is damaged. It never gains an installation or consent token.
      if (foreignPackageFormat(a.path)) return inspectForeignPackageFile(a.path);
      const source = a.path ? await boundedRead(a.path) : a.source;
      const compatibility = inspectForeignManifest(source);
      if (compatibility) return compatibility;
      return JSON.parse(source).packageFormat === "pcl-linux.declarative-package"
        ? extensions.preparePackageReview(source)
        : extensions.prepareReview(source);
    }
    case "extensions_confirm":
      return extensions.confirmReview(a.token, a.grants);
    case "extensions_cancel":
      return extensions.cancelReview(a.token);
    case "extensions_revoke":
      return extensions.revoke(a.id, a.capability, a.expected);
    case "extensions_enable":
      return extensions.enable(a.id, a.expected);
    case "extensions_disable":
      return extensions.disable(a.id, a.expected);
    case "extensions_remove":
      return extensions.remove(a.id, a.expected);
    case "extensions_rereview":
      return extensions.rereview(a.id);
    case "extensions_safe_mode":
      return extensions.setSafeMode(a.enabled);
    case "extensions_settings":
      return extensions.updateSettings(a.id, a.values, a.expected);
    case "extensions_cards":
      extensions.setContext(a.context ?? null);
      return extensions.cards(a.slot, a.filter);
    case "extensions_catalog":
      extensions.setContext(a.context ?? null);
      return extensions.catalog();
    case "extensions_action":
      extensions.setContext(a.context ?? null);
      return extensions.commitAction(
        extensions.prepareAction(a.extensionId, a.cardId, a.actionId, a.expected).token,
      );
    case "idle_prepare":
      // Safe mode and installed grants are durable. Only an unconsumed review
      // needs this lightweight process to stay alive before AI is loaded.
      // Return a deferred native locator until Agent has accepted it, including
      // across plugin-only wakeups followed by another process suspension.
      if (!agent) return extensions.pendingReviewCount
        ? { busy: true }
        : { busy: false, resumePresetId };
      return agent.dispatch(operation, a);
    case "idle_resume":
      if (agent) return agent.dispatch(operation, a);
      resumePresetId = typeof a.presetId === "string" ? a.presetId : null;
      return true;
    case "source_release":
      if (
        !a ||
        Object.keys(a).join() !== "sourceId" ||
        typeof a.sourceId !== "string" ||
        !/^[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12}$/.test(a.sourceId)
      )
        throw new Error("Invalid source release token");
      // Cleanup cannot initialize Agent/project services on a plugin-only host.
      return agent ? agent.dispatch(operation, a) : false;
    case "shutdown":
      closing = true;
      return agent ? agent.dispatch(operation, a) : true;
    default:
      if (typeof operation === "string" && operation.startsWith("extensions_"))
        throw new Error("Unknown experimental host operation");
      return (await ensureAgent()).dispatch(operation, a);
  }
}
// Native calls are serialized, but runtime jobs continue independently. Every
// line receives one bounded envelope; no model or child stdout reaches IPC.
const input = readline.createInterface({
  input: process.stdin,
  crlfDelay: Infinity,
});
for await (const line of input) {
  let reply,
    replyLimit = 1000000;
  try {
    if (Buffer.byteLength(line) > 24 * 1024 * 1024)
      throw new Error("Request exceeds native snapshot limit");
    const request = JSON.parse(line);
    if (request.operation === "rh_code_harness_export")
      replyLimit = 24 * 1024 * 1024;
    const requestLimit = request.operation === "extensions_settings" ? 512 * 1024 : 128000;
    if (request.operation !== "rh_code_run" && Buffer.byteLength(line) > requestLimit)
      throw new Error("Request exceeds the operation input limit");
    reply = {
      ok: true,
      result: await dispatch(request.operation, request.args ?? {}),
    };
  } catch (error) {
    reply = { ok: false, error: String(error.message).slice(0, 1000) };
  }
  const output = JSON.stringify(reply);
  process.stdout.write(
    (Buffer.byteLength(output) <= replyLimit
      ? output
      : JSON.stringify({ ok: false, error: "Host response exceeds limit" })) +
      "\n",
  );
  if (closing) break;
}
if (!closing) await dispatch("shutdown", {});
