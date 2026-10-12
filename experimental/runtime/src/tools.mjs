import { check, RuntimeError, cloneJson, throwIfAborted } from './errors.mjs';

export class ToolRegistry {
  #tools = new Map();
  register(tool) {
    check(tool && tool.name?.length <= 80 && /^[a-z][a-z0-9_]*\.[a-z][a-z0-9_]*$/.test(tool.name), 'INVALID_TOOL', 'Tool needs a short namespaced name');
    check(!this.#tools.has(tool.name), 'DUPLICATE_TOOL', `Duplicate tool ${tool.name}`);
    check(typeof tool.execute === 'function' && typeof tool.validate === 'function', 'INVALID_TOOL', 'Tool needs validation and execution');
    check(['read-only', 'idempotent', 'manual'].includes(tool.replaySafety), 'INVALID_TOOL', 'Declare tool replay safety');
    check(tool.projectReceipt === undefined || typeof tool.projectReceipt === 'function', 'INVALID_TOOL', 'Receipt projection must be a host function');
    check(tool.replayCompleted === undefined || (typeof tool.replayCompleted === 'boolean' && (!tool.replayCompleted || tool.replaySafety === 'read-only')), 'INVALID_TOOL', 'Only read-only tools may reconstruct completed results');
    this.#tools.set(tool.name, Object.freeze({ ...tool })); return this;
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
