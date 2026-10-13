/** Single-purpose native helper transport. The trusted native host supplies its
 * own executable; no model input enters executable/argv/environment selection.
 * Closing stdin cancels the helper, which must reap its sandbox before exit. */
import { spawn } from "node:child_process";
import path from "node:path";
import { StringDecoder } from "node:string_decoder";
import { check, throwIfAborted } from "../experimental/runtime/src/errors.mjs";
import { BUILD_LIMITS } from "./build-snapshot.mjs";

const TERMINAL = new Set(["completed", "failed", "cancelled", "timed_out", "output_limit", "blocked"]);
const failure = (code, message) => ({ status: "interrupted", exitCode: null, signal: null, error: { code, message } });
export class NativeBuildRunner {
  #executable; #args; #active = new Set(); #closed = false; #cancelGrace;
  constructor({ executable = process.env.PCL_RH_BUILD_RUNNER_EXE, args = ["--rh-code-build-runner"], cancelGraceMs = 3000 } = {}) {
    check((executable === undefined || (typeof executable === "string" && path.isAbsolute(executable))) &&
      Array.isArray(args) && args.every((value) => typeof value === "string") &&
      Number.isInteger(cancelGraceMs) && cancelGraceMs > 0 && cancelGraceMs <= 10_000,
    "INVALID_BUILD_RUNNER", "Expected a trusted helper executable and bounded cancellation grace");
    this.#executable = executable; this.#args = [...args]; this.#cancelGrace = cancelGraceMs;
  }
  hasActiveWork() { return this.#active.size !== 0; }
  async run(request, { signal, onFrame = async () => {} } = {}) {
    throwIfAborted(signal);
    check(!this.#closed, "BUILD_HOST_CLOSED", "Build helper owner is closing");
    if (!this.#executable || process.platform !== "linux") return { status: "blocked", exitCode: null, signal: null,
      error: { code: "BUILD_RUNNER_UNAVAILABLE", message: "The native contained build helper is unavailable on this host." } };
    const input = JSON.stringify(request);
    check(Buffer.byteLength(input) <= 16_000, "BUILD_REQUEST_LIMIT", "Build helper request exceeded its wire budget");
    const child = spawn(this.#executable, this.#args, { detached: true, shell: false,
      stdio: ["pipe", "pipe", "pipe"], cwd: request.root,
      env: { PATH: "/usr/bin:/bin", LANG: "C.UTF-8" } });
    const owner = { cancel: null, done: null }; this.#active.add(owner);
    owner.done = new Promise((resolve) => {
      const decoder = new StringDecoder("utf8");
      let pending = "", wireBytes = 0, outputBytes = 0, started = false, finished = null;
      let fault = null, forced = false, cancelling = false, frameWrites = Promise.resolve(), escalation;
      const killGroup = () => {
        if (child.pid) try { process.kill(-child.pid, "SIGKILL"); } catch (error) { if (error.code !== "ESRCH") fault ??= failure("BUILD_CLEANUP_FAILED", "The build helper process group could not be stopped."); }
        child.stdin.destroy(); child.stdout.destroy(); child.stderr.destroy();
      };
      const cancel = () => {
        if (cancelling) return;
        cancelling = true;
        // The helper retains this control pipe through sandbox cleanup. SIGKILL
        // is an uncertain fallback and can never produce a success receipt.
        child.stdin.end('{"type":"cancel"}\n');
        escalation = setTimeout(() => { forced = true; killGroup(); }, this.#cancelGrace);
      };
      const fail = (code, message) => { fault ??= failure(code, message); cancel(); };
      owner.cancel = cancel;
      const onAbort = () => cancel();
      signal?.addEventListener("abort", onAbort, { once: true });
      const deadline = setTimeout(() => fail("BUILD_HELPER_DEADLINE", "Build helper cleanup exceeded the captured deadline."), request.timeoutMs + this.#cancelGrace);
      const frame = (line) => {
        if (fault) return;
        let value;
        try { value = JSON.parse(line); }
        catch { fail("BUILD_PROTOCOL_ERROR", "The native build helper returned an invalid control frame."); return; }
        if (!value || value.schemaVersion !== 1 || value.runId !== request.runId || finished) {
          fail("BUILD_PROTOCOL_ERROR", "The build helper frame has an unexpected identity or ordering."); return;
        }
        if (value.type === "started" && !started) started = true;
        else if (value.type === "output" && started && ["stdout", "stderr"].includes(value.stream) &&
          typeof value.text === "string" && value.text.isWellFormed()) {
          outputBytes += Buffer.byteLength(value.text);
          if (outputBytes > request.maxOutputBytes) { fail("BUILD_PROTOCOL_ERROR", "Build helper output exceeded its captured budget."); return; }
        } else if (value.type === "finished" && TERMINAL.has(value.status) &&
          (value.status !== "completed" || (started && value.exitCode === 0)) &&
          (value.exitCode === undefined || value.exitCode === null || Number.isInteger(value.exitCode)) &&
          (value.signal === undefined || value.signal === null || typeof value.signal === "string")) {
          const error = value.error == null ? null : typeof value.error === "string"
            ? { code: "BUILD_RUNNER_ERROR", message: value.error.slice(0, 1000) }
            : typeof value.error.code === "string" && typeof value.error.message === "string"
              ? { code: value.error.code.slice(0, 80), message: value.error.message.slice(0, 1000) } : null;
          finished = { status: value.status, exitCode: value.exitCode ?? null, signal: value.signal ?? null, error };
        } else { fail("BUILD_PROTOCOL_ERROR", "The build helper returned an unsupported control frame."); return; }
        frameWrites = frameWrites.then(() => onFrame(value)).catch(() => fail("BUILD_LOG_WRITE_FAILED", "The private build log or run receipt could not be saved."));
      };
      child.stdout.on("data", (bytes) => {
        wireBytes += bytes.length;
        if (wireBytes > BUILD_LIMITS.outputBytes * 8) { fail("BUILD_PROTOCOL_ERROR", "Build helper framing exceeded its wire budget."); return; }
        pending += decoder.write(bytes);
        let newline;
        while ((newline = pending.indexOf("\n")) >= 0) {
          const line = pending.slice(0, newline); pending = pending.slice(newline + 1);
          if (Buffer.byteLength(line) > 256_000) { fail("BUILD_PROTOCOL_ERROR", "Build helper control frame exceeded its limit."); break; }
          frame(line);
        }
        if (Buffer.byteLength(pending) > 256_000) fail("BUILD_PROTOCOL_ERROR", "Build helper control frame exceeded its limit.");
      });
      // Helper diagnostics never enter the launcher IPC or an unbounded buffer.
      child.stderr.resume();
      child.once("error", () => { fault = { status: "blocked", exitCode: null, signal: null,
        error: { code: "BUILD_RUNNER_UNAVAILABLE", message: "The trusted native build helper could not be started." } }; });
      child.stdin.on("error", (error) => { if (error.code !== "EPIPE") fail("BUILD_CONTROL_FAILED", "Build helper control pipe failed."); });
      child.once("exit", () => {
        // stdout can still contain its final frame after the exit notification.
        // Give the pipe a bounded drain before treating an unconfirmed exit as
        // an orphaned helper whose inherited pipes could otherwise hang close.
        if (!finished) escalation ??= setTimeout(() => { if (!finished) { forced = true; killGroup(); } }, 100);
      });
      child.once("close", async (code) => {
        clearTimeout(deadline); clearTimeout(escalation); signal?.removeEventListener("abort", onAbort);
        pending += decoder.end();
        if (pending.trim()) fault ??= failure("BUILD_PROTOCOL_ERROR", "The build helper ended with an incomplete control frame.");
        await frameWrites;
        clearTimeout(escalation);
        const result = forced ? failure("BUILD_CLEANUP_UNCONFIRMED", "Build helper cancellation required forced termination; cleanup is unconfirmed.")
          : fault ?? (!finished || code !== (finished.status === "completed" ? 0 : 1)
            ? failure("BUILD_HELPER_INTERRUPTED", "The build helper exited without a confirmed cleanup receipt.") : finished);
        resolve(result);
      });
      child.stdin.write(`${input}\n`);
      if (signal?.aborted || this.#closed) cancel();
    });
    try { return await owner.done; } finally { this.#active.delete(owner); }
  }
  async shutdown() {
    this.#closed = true;
    const owners = [...this.#active]; owners.forEach((owner) => owner.cancel());
    await Promise.allSettled(owners.map((owner) => owner.done));
  }
}
