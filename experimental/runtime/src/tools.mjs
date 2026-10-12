import { check, RuntimeError, cloneJson, throwIfAborted } from './errors.mjs';

export class ToolRegistry {
  #tools = new Map();
  #resolvers = new Map();
  register(tool) {
    check(tool && tool.name?.length <= 80 && /^[a-z][a-z0-9_]*\.[a-z][a-z0-9_]*$/.test(tool.name), 'INVALID_TOOL', 'Tool needs a short namespaced name');
    check(!this.#tools.has(tool.name), 'DUPLICATE_TOOL', `Duplicate tool ${tool.name}`);
    check(typeof tool.execute === 'function' && typeof tool.validate === 'function', 'INVALID_TOOL', 'Tool needs validation and execution');
    check(['read-only', 'idempotent', 'manual'].includes(tool.replaySafety), 'INVALID_TOOL', 'Declare tool replay safety');
    check(tool.projectReceipt === undefined || typeof tool.projectReceipt === 'function', 'INVALID_TOOL', 'Receipt projection must be a host function');
    check(tool.replayCompleted === undefined || (typeof tool.replayCompleted === 'boolean' && (!tool.replayCompleted || tool.replaySafety === 'read-only')), 'INVALID_TOOL', 'Only read-only tools may reconstruct completed results');
    // A plugin may retire while a consumer retains this descriptor. The guard
    // revokes that old function too, rather than only removing a map entry.
    const name = tool.name, execute = tool.execute;
    const registered = Object.freeze({ ...tool, execute: (...args) => {
      check(this.#tools.get(name) === registered, 'TOOL_UNAVAILABLE', `Tool ${name} registration retired`);
      return execute.apply(registered, args);
    } });
    this.#tools.set(tool.name, registered); return this;
  }
  contribute(tool) {
    this.register(tool);
    const registered = this.#tools.get(tool.name);
    return () => { if (this.#tools.get(tool.name) === registered) this.#tools.delete(tool.name); };
  }
  setScopeResolver(profile, resolve) {
    check(typeof profile === 'string' && typeof resolve === 'function' && !this.#resolvers.has(profile),
      'INVALID_TOOL_SCOPE', 'A trusted profile has one tool scope resolver');
    this.#resolvers.set(profile, resolve);
    return () => { if (this.#resolvers.get(profile) === resolve) this.#resolvers.delete(profile); };
  }
  resolveAllowed(job, fallback) {
    const resolve = this.#resolvers.get(job.profile);
    const names = resolve ? resolve(job) : fallback;
    check(Array.isArray(names) && names.length <= 128 && new Set(names).size === names.length &&
      names.every(name => typeof name === 'string'), 'INVALID_TOOL_SCOPE', 'Invalid host tool scope');
    return [...names];
  }
  get(name, allowed) {
    check(allowed.includes(name), 'TOOL_DENIED', `Tool ${name} is not allowed in this profile`);
    const tool = this.#tools.get(name);
    check(tool, 'TOOL_UNAVAILABLE', `Tool ${name} has no registered implementation`);
    return tool;
  }
  describe(allowed) {
    return allowed.filter(n => this.#tools.has(n)).map(name => {
      const { description, inputSchema } = this.#tools.get(name);
      return { name, description, inputSchema };
    });
  }
}
function objectKeys(value, keys) {
  check(value && typeof value === 'object' && !Array.isArray(value), 'INVALID_ARGUMENTS', 'Expected an object');
  check(Object.keys(value).every(key => keys.includes(key)), 'INVALID_ARGUMENTS', 'Unknown input field');
}
export function createDefaultTools() {
  return new ToolRegistry()
    .register({ name: 'workspace.read', description: 'Read text in this job workspace', replaySafety: 'read-only',
      inputSchema: { type: 'object', properties: { path: { type: 'string' } }, required: ['path'], additionalProperties: false },
      validate: a => { objectKeys(a, ['path']); check(typeof a.path === 'string', 'INVALID_ARGUMENTS', 'path is required'); },
      execute: (a, ctx) => { throwIfAborted(ctx.signal); return ctx.workspace.read(a.path); } })
    .register({ name: 'workspace.write', description: 'Write notes in scratch/** only. Generated, reviewed and report artifacts are host-owned and immutable to this tool.', replaySafety: 'idempotent',
      inputSchema: { type: 'object', properties: { path: { type: 'string' }, content: { type: 'string' } }, required: ['path', 'content'], additionalProperties: false },
      validate: a => {
        objectKeys(a, ['path', 'content']); check(typeof a.path === 'string' && typeof a.content === 'string', 'INVALID_ARGUMENTS', 'path and content are required');
        check(!a.path.startsWith('/') && !/[\\\x00-\x1f]/.test(a.path) && a.path.split('/').every(part => part && part !== '.' && part !== '..'), 'UNSAFE_PATH', 'Expected a canonical relative path');
        check(a.path.startsWith('scratch/') && a.path.length > 8, 'WORKSPACE_WRITE_DENIED', 'Generic writes are restricted to scratch/**; domain and review artifacts are host-owned');
      },
      execute: (a, ctx) => { throwIfAborted(ctx.signal); return ctx.workspace.write(a.path, a.content); } })
    .register({ name: 'build.validate', description: 'Blocked: generated-code execution awaits tested containment', replaySafety: 'manual',
      inputSchema: { type: 'object', properties: {}, additionalProperties: false },
      validate: a => objectKeys(a, []),
      execute: () => { throw new RuntimeError('BUILD_EXECUTION_DISABLED', 'Generated-code execution is disabled until OS containment is implemented and tested'); } });
}
export function safeToolResult(result) {
  return cloneJson(result, { maxBytes: 64_000, code: 'TOOL_RESULT_TOO_LARGE' });
}
