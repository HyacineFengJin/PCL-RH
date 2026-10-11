import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import path from 'node:path';
import { SubprocessSupervisor } from '../subprocesses.mjs';
import { check, cloneJson, RuntimeError, throwIfAborted } from '../errors.mjs';

const NAMES = ['maker.validate', 'maker.plan', 'maker.generate', 'porter.inspect', 'porter.plan', 'porter.validate'];
function validateObject(args, allowed) {
  check(args && typeof args === 'object' && !Array.isArray(args), 'INVALID_ARGUMENTS', 'Expected a domain argument object');
  check(Object.keys(args).every(key => allowed.includes(key)), 'INVALID_ARGUMENTS', 'Unknown domain argument field');
  cloneJson(args, { maxBytes: 120_000, code: 'DOMAIN_INPUT_TOO_LARGE' });
}

export function registerDomainAdapters(registry, { pythonExecutable = '/usr/bin/python3' } = {}) {
  check(path.isAbsolute(pythonExecutable), 'INVALID_INTERPRETER', 'Host must choose an absolute Python interpreter path');
  const bridge = fileURLToPath(new URL('./python_bridge.py', import.meta.url));
  const supervisor = new SubprocessSupervisor({ trustedCommands: Object.fromEntries(NAMES.map(name => [name,
    { executable: pythonExecutable, args: ['-I', '-B', bridge, name] }])) });
  for (const name of NAMES) {
    const maker = name.startsWith('maker.');
    registry.register({ name, replaySafety: name === 'maker.validate' || name === 'maker.plan' ? 'read-only' : 'idempotent',
      description: maker ? 'Deterministic source/spec operation. No AI, build or game execution.' : 'Deterministic read-only port analysis. Validation refers to inputs/report shape, never compatibility.',
      inputSchema: maker ? { type: 'object', properties: { spec: { type: 'object' } }, required: ['spec'], additionalProperties: false }
        : { type: 'object', properties: { files: { type: 'object', additionalProperties: { type: 'string' } }, targetId: { type: 'string' }, rights: { type: 'string', enum: ['unknown', 'owner', 'permission', 'license-reviewed'] }, acknowledgeBeta: { type: 'boolean' } }, required: ['files'], additionalProperties: false },
      validate: args => {
        validateObject(args, maker ? ['spec'] : ['files', 'targetId', 'rights', 'acknowledgeBeta']);
        check((maker ? args.spec : args.files) && typeof (maker ? args.spec : args.files) === 'object' && !Array.isArray(maker ? args.spec : args.files), 'INVALID_ARGUMENTS', maker ? 'spec is required' : 'files map is required');
      },
      async execute(args, ctx) {
        throwIfAborted(ctx.signal);
        const run = await supervisor.run(name, { cwd: ctx.workspace.root, signal: ctx.signal,
          input: JSON.stringify(args), maxOutputBytes: 256_000, timeoutMs: 5000 });
        throwIfAborted(ctx.signal);
        let envelope;
        try { envelope = JSON.parse(run.stdout); } catch {
          throw new RuntimeError('DOMAIN_PROCESS_FAILED', `Trusted domain bridge returned ${run.status} without a bounded JSON result`);
        }
        check(run.status === 'completed' && envelope.ok, 'DOMAIN_REJECTED', envelope.error?.message ?? `Trusted domain bridge returned ${run.status}`);
        const result = envelope.result;
        if (name === 'maker.generate') {
          check(Array.isArray(result.files) && result.files.length <= 300, 'INVALID_DOMAIN_RESULT', 'Invalid generated file bundle');
          const artifacts = [];
          const outputDirectory = `generated/${ctx.operationId}`;
          for (const file of result.files) {
            throwIfAborted(ctx.signal);
            check(typeof file.path === 'string' && typeof file.base64 === 'string', 'INVALID_DOMAIN_RESULT', 'Invalid generated file');
            const bytes = Buffer.from(file.base64, 'base64');
            const artifact = await ctx.workspace.writeBytes(`${outputDirectory}/${file.path}`, bytes);
            artifacts.push({ ...artifact, sha256: createHash('sha256').update(bytes).digest('hex') });
          }
          return { status: 'source_generated_only', provider: 'deterministic_offline', outputDirectory, artifacts, build: 'not_run', modelCalls: 0 };
        }
        if (!maker) {
          const suffix = name.split('.')[1];
          const reportPath = `reports/${ctx.operationId}-porter-${suffix}.json`;
          await ctx.workspace.write(reportPath, JSON.stringify(result, null, 2));
          return { status: result.status, reportPath, inputFingerprint: result.input_fingerprint,
            sourceFiles: result.source.source_files, filesAnalyzed: result.source.files_analyzed,
            target: result.target.id, blockers: result.blockers.map(b => ({ code: b.code, title: b.title })),
            reportContractChecked: name === 'porter.validate', buildValidated: false,
            execution: result.execution, validation: result.validation_matrix };
        }
        return result;
      },
    });
  }
  return { registry, hasActiveWork: () => supervisor.hasActiveWork(), shutdown: () => supervisor.shutdown() };
}
