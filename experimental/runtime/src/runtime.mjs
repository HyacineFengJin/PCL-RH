import { createHash } from 'node:crypto';
import { JobStore } from './store.mjs';
import { PROFILES } from './profiles.mjs';
import { createDefaultTools, safeToolResult } from './tools.mjs';
import { DeterministicFakeProvider } from './providers/fake.mjs';
import { check, cloneJson, throwIfAborted } from './errors.mjs';
import { projectPublicJobSummary } from './public-summary.mjs';

const TERMINAL = new Set(['completed', 'cancelled', 'failed', 'interrupted']);
function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (value && typeof value === 'object') return `{${Object.keys(value).sort().map(k => `${JSON.stringify(k)}:${canonical(value[k])}`).join(',')}}`;
  return JSON.stringify(value);
}
export class AgentRuntime {
  #active = new Map();
  constructor({ root, tools = createDefaultTools(), providers = [new DeterministicFakeProvider()], maxConcurrentJobs = 2 }) {
    this.store = new JobStore(root); this.tools = tools;
    this.providers = new Map(providers.map(provider => [provider.id, provider]));
    check(Number.isInteger(maxConcurrentJobs) && maxConcurrentJobs > 0 && maxConcurrentJobs <= 8, 'INVALID_LIMIT', 'Concurrency must be 1–8');
    this.maxConcurrentJobs = maxConcurrentJobs;
  }
  async init() { await this.store.init(); return this; }
  async createJob({ profile, input = {}, provider = 'deterministic-fake', providerConfig = {} }) {
    check(typeof profile === 'string' && Object.hasOwn(PROFILES, profile), 'INVALID_PROFILE', 'Choose maker or porter');
    check(this.providers.has(provider), 'PROVIDER_UNAVAILABLE', `Provider ${provider} is not configured`);
    return this.store.create({ profile,
      input: cloneJson(input, { maxBytes: 64_000, code: 'INPUT_TOO_LARGE' }), provider,
      providerConfig: cloneJson(providerConfig, { maxBytes: 128_000, code: 'PROVIDER_CONFIG_TOO_LARGE' }) });
  }
  getJob(id) { return this.store.load(id); }
  hasActiveJob(id) { return this.#active.has(id); }
  hasActiveWork() { return this.#active.size !== 0; }
  async getPublicJobSummary(id) { return projectPublicJobSummary(await this.getJob(id)); }
  start(id, { resume = false } = {}) {
    check(!this.#active.has(id), 'JOB_BUSY', 'Job is already active');
    check(this.#active.size < this.maxConcurrentJobs, 'CAPACITY', 'Runtime concurrency limit reached');
    const run = { controller: new AbortController(), finishing: false };
    this.#active.set(id, run);
    run.promise = this.#run(id, run, resume).finally(() => this.#active.delete(id));
    return run.promise;
  }
  resume(id) { return this.start(id, { resume: true }); }
  async cancel(id) {
    const active = this.#active.get(id);
    if (active) {
      if (active.finishing) return false;
      active.controller.abort();
      await active.promise;
      return true;
    }
    const release = await this.store.acquire(id);
    try {
      const job = await this.getJob(id);
      if (TERMINAL.has(job.status)) return false;
      job.status = 'cancelled'; await this.store.save(job); await this.store.event(id, 'job.cancelled');
      return true;
    } finally { await release(); }
  }
  async shutdown() {
    const runs = [...this.#active.values()];
    for (const run of runs) run.controller.abort();
    await Promise.allSettled(runs.map(run => run.promise));
  }
  async #run(id, run, resume) {
    const release = await this.store.acquire(id);
    let job;
    try {
      job = await this.getJob(id);
      check(job.status !== 'completed', 'JOB_COMPLETED', 'Completed jobs cannot be resumed');
      check(job.status === 'queued' || resume, 'RESUME_REQUIRED', 'Explicit resume is required for previous work');
      const profile = Object.hasOwn(PROFILES, job.profile) ? PROFILES[job.profile] : undefined;
      const provider = this.providers.get(job.provider);
      check(profile && provider, 'PROVIDER_UNAVAILABLE', 'Persisted profile/provider is unavailable');
      job.status = 'running'; job.attempt++; job.error = null;
      await this.store.save(job); await this.store.event(id, 'job.started', { attempt: job.attempt });
      // Permission modes are captured by the trusted host, not by model text.
      // Other workflows and older Maker jobs keep their original tool contract.
      const intent = job.profile === 'maker' ? job.input.makerThread?.mode : null;
      check(!intent || ['ask', 'plan', 'execute'].includes(intent), 'INVALID_MODE', 'Unsupported Maker mode');
      const allowedTools = intent && intent !== 'execute'
        ? profile.tools.filter(name => ['workspace.read', 'maker.plan', 'maker.report_plan'].includes(name))
        : profile.tools.filter(name => !(intent && job.input.sourceOperationId && name === 'maker.generate'));
      const signal = run.controller.signal;
      const workspace = await this.store.workspace(id);
      // Providers may supply a captured host budget; persisted job input has
      // no authority to remove a count limit. Receipt byte limits remain separate.
      const toolOperationLimit = provider.getToolOperationLimit?.(job) ?? 200;
      check(Number.isSafeInteger(toolOperationLimit) && toolOperationLimit >= 0, 'INVALID_LIMIT', 'Invalid provider tool operation limit');
      // Serial host-tool execution avoids same-workspace mutations racing each other.
      let queue = Promise.resolve();
      const serial = operation => {
        const next = queue.then(operation); queue = next.catch(() => {}); return next;
      };
      const ctx = {
        job: cloneJson(job), profile, signal, workspaceRoot: workspace.root,
        toolDescriptions: this.tools.describe(allowedTools),
        checkpoint: state => serial(async () => {
          throwIfAborted(signal);
          const nextState = cloneJson(state, { maxBytes: 64_000, code: 'CHECKPOINT_TOO_LARGE' });
          job.providerState = nextState; await this.store.save(job);
        }),
        emit: (type, metadata) => this.store.event(id, type, metadata),
        invoke: (operationId, name, input, { signal: callSignal } = {}) => serial(async () => {
          const toolSignal = callSignal ? AbortSignal.any([signal, callSignal]) : signal;
          throwIfAborted(toolSignal);
          check(typeof operationId === 'string' && /^[a-zA-Z0-9_-]{1,120}(?![\s\S])/.test(operationId), 'INVALID_OPERATION', 'Tool operation needs a stable ID');
          const tool = this.tools.get(name, allowedTools);
          const args = cloneJson(input); tool.validate(args);
          if (intent && name === 'maker.preview_source_edit') {
            const protectedPaths = job.input.makerThread.protectedPaths ?? [];
            check(!args.request.edits.some(edit=>protectedPaths.includes(edit.path)), 'HANDWRITTEN_PROTECTED', 'Agent cannot edit handwritten protected files; choose an unprotected implementation path');
          }
          if (intent === 'execute' && ['maker.generate', 'maker.preview_revision', 'maker.preview_source_edit'].includes(name))
            check(job.operations.some(o => o.name === 'maker.report_plan' && o.status === 'completed'), 'PLAN_REQUIRED', 'Record a plan before source changes');
          const fingerprint = createHash('sha256').update(canonical({ name, args })).digest('hex');
          let operation = job.operations.find(o => o.id === operationId);
          if (operation) {
            check(operation.fingerprint === fingerprint, 'OPERATION_MISMATCH', 'Operation ID was reused with different arguments');
            if (operation.status === 'completed') return cloneJson(operation.result);
            check(tool.replaySafety !== 'manual', 'RECONCILIATION_REQUIRED', 'An incomplete non-idempotent tool needs manual reconciliation');
          } else {
            check(toolOperationLimit === 0 || job.operations.length < toolOperationLimit, 'TOOL_LIMIT', 'Job exceeded its tool operation budget');
            operation = { id: operationId, name, fingerprint, status: 'running', ...(intent && name === 'workspace.read' ? { path: args.path } : {}) };
            cloneJson([...job.operations, operation], { maxBytes: 128_000, code: 'RECEIPT_BUDGET' });
            job.operations.push(operation);
          }
          operation.status = 'running';
          await this.store.save(job); await this.store.event(id, 'tool.started', { operationId, name });
          const result = safeToolResult(await tool.execute(args, { workspace, signal: toolSignal, jobId: id, operationId, profile: profile.id }));
          // Save the receipt even if cancellation arrived after the effect completed.
          const proposed = job.operations.map(o => o === operation ? { ...o, status: 'completed', result } : o);
          cloneJson(proposed, { maxBytes: 128_000, code: 'RECEIPT_BUDGET' });
          operation.status = 'completed'; operation.result = result;
          await this.store.save(job); await this.store.event(id, 'tool.completed', { operationId, name });
          throwIfAborted(toolSignal);
          return cloneJson(result);
        }),
      };
      throwIfAborted(signal);
      try { job.result = cloneJson(await provider.run(ctx), { maxBytes: 64_000, code: 'RESULT_TOO_LARGE' }); }
      finally { await queue; }
      throwIfAborted(signal);
      run.finishing = true;
      job.status = 'completed';
      await this.store.save(job); await this.store.event(id, 'job.completed', { buildValidated: false });
      return cloneJson(job);
    } catch (error) {
      if (!job || ['JOB_COMPLETED', 'RESUME_REQUIRED', 'PROVIDER_UNAVAILABLE'].includes(error.code)) throw error;
      run.finishing = true;
      job.status = run.controller.signal.aborted ? 'cancelled' : 'failed';
      job.error = { code: error.code ?? 'RUNTIME_ERROR', message: String(error.message).slice(0, 1000) };
      await this.store.save(job); await this.store.event(id, `job.${job.status}`, { code: job.error.code });
      return cloneJson(job);
    } finally { await release(); }
  }
}
