import { spawn } from 'node:child_process';
import { check, throwIfAborted } from './errors.mjs';

// Host-only lifecycle utility. Never registered as an agent tool. An executable
// allowlist is NOT a sandbox; generated-code/build execution remains disabled.
export class SubprocessSupervisor {
  #commands;
  #active = new Set();
  hasActiveWork() { return this.#active.size !== 0; }
  constructor({ trustedCommands = {} } = {}) {
    this.#commands = new Map(Object.entries(trustedCommands).map(([name, spec]) => {
      check(typeof spec.executable === 'string' && Array.isArray(spec.args) && spec.args.every(a => typeof a === 'string'),
        'INVALID_COMMAND', 'Trusted host commands need a fixed executable and argument list');
      return [name, { executable: spec.executable, args: [...spec.args] }];
    }));
  }
  async run(name, { cwd, signal, input = '', timeoutMs = 10_000, maxOutputBytes = 64_000, onOutput = () => {} } = {}) {
    check(process.platform === 'linux', 'PLATFORM_UNVERIFIED', 'Process-group cleanup has only been tested on Linux');
    check(this.#commands.has(name), 'PROCESS_DENIED', 'Command is not a host-registered trusted fixture');
    check(this.#active.size < 4, 'PROCESS_CAPACITY', 'Too many supervised processes');
    check(Number.isInteger(timeoutMs) && timeoutMs > 0 && timeoutMs <= 60_000, 'INVALID_TIMEOUT', 'Timeout must be 1–60000 ms');
    check(Number.isInteger(maxOutputBytes) && maxOutputBytes > 0 && maxOutputBytes <= 256_000, 'INVALID_OUTPUT_LIMIT', 'Invalid output limit');
    check(typeof input === 'string' && Buffer.byteLength(input) <= 128_000, 'PROCESS_INPUT_LIMIT', 'Trusted command input exceeds 128 KB');
    throwIfAborted(signal);
    const control = new AbortController();
    const combined = signal ? AbortSignal.any([signal, control.signal]) : control.signal;
    const command = this.#commands.get(name);
    const child = spawn(command.executable, command.args, {
      cwd, detached: true, shell: false, stdio: ['pipe', 'pipe', 'pipe'],
      // Do not leak API keys, account tokens or the user's ambient environment.
      env: { PATH: process.env.PATH ?? '/usr/bin:/bin', LANG: 'C.UTF-8' },
    });
    const record = { control, done: null };
    this.#active.add(record);
    record.done = new Promise(resolve => {
      let reason; let escalation; let spawnError;
      const chunks = { stdout: [], stderr: [] }; let outputBytes = 0;
      const killGroup = sig => {
        if (!child.pid) return;
        try { process.kill(-child.pid, sig); } catch (error) { if (error.code !== 'ESRCH') spawnError ??= error; }
      };
      const stop = why => {
        if (reason) return;
        reason = why; killGroup('SIGTERM');
        escalation = setTimeout(() => killGroup('SIGKILL'), 100);
      };
      const onAbort = () => stop('cancelled');
      combined.addEventListener('abort', onAbort, { once: true });
      if (combined.aborted) onAbort();
      const timeout = setTimeout(() => stop('timed_out'), timeoutMs);
      for (const stream of ['stdout', 'stderr']) child[stream].on('data', data => {
        const remaining = Math.max(0, maxOutputBytes - outputBytes);
        const bounded = data.subarray(0, remaining); outputBytes += bounded.length;
        chunks[stream].push(bounded);
        try { onOutput(stream, bounded.toString('utf8')); }
        catch (error) { spawnError ??= error; stop('callback_failed'); }
        if (data.length > remaining) stop('output_limit');
      });
      child.once('error', error => { spawnError = error; });
      child.stdin.on('error', error => { if (error.code !== 'EPIPE') { spawnError ??= error; stop('input_failed'); } });
      child.stdin.end(input);
      // Kill descendants even if the initial child exits first and leaves pipes open.
      child.once('exit', () => killGroup('SIGKILL'));
      child.once('close', (code, exitSignal) => {
        clearTimeout(timeout); clearTimeout(escalation);
        combined.removeEventListener('abort', onAbort);
        resolve({ status: reason ?? (spawnError || code !== 0 ? 'failed' : 'completed'),
          exitCode: code, signal: exitSignal,
          stdout: Buffer.concat(chunks.stdout).toString('utf8'), stderr: Buffer.concat(chunks.stderr).toString('utf8'),
          error: spawnError ? { code: spawnError.code ?? 'PROCESS_ERROR', message: String(spawnError.message).slice(0, 1000) } : null });
      });
    });
    try { return await record.done; } finally { this.#active.delete(record); }
  }
  async shutdown() {
    const records = [...this.#active];
    records.forEach(record => record.control.abort());
    await Promise.all(records.map(record => record.done));
  }
}
