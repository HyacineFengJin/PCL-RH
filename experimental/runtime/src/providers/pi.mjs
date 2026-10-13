import path from 'node:path';
import { createHash } from 'node:crypto';
import { check, RuntimeError, throwIfAborted } from '../errors.mjs';

export const PINNED_PI = Object.freeze({ package: '@earendil-works/pi-coding-agent', version: '1.0.4' });

// Inspection only: no session, model request, auth discovery or account action.
export async function inspectInstalledPi() {
  const sdk = await import('@earendil-works/pi-coding-agent');
  check(sdk.VERSION === PINNED_PI.version, 'SDK_VERSION_MISMATCH', 'Installed Pi version differs from the audited pin');
  for (const name of ['createAgentSession', 'DefaultResourceLoader', 'SettingsManager', 'SessionManager']) {
    check(typeof sdk[name] === 'function', 'SDK_CONTRACT_CHANGED', `Missing SDK export ${name}`);
  }
  return { ...PINNED_PI, exportsVerified: true, sessionCreated: false, modelCalls: 0 };
}

// Intentionally no boolean flag can enable paid/live calls in this milestone.
export class DisabledPiProvider {
  id = 'pi';
  async run() {
    throw new RuntimeError('LIVE_MODEL_CALLS_DISABLED', 'Pi SDK is pinned; live session/auth integration requires a separate approved step');
  }
}

export function projectPiToolResult(name, result, supportsImages = false) {
  if (!/^(pixel|model)\.(create|edit|inspect)$/.test(name))
    return { content: [{ type: 'text', text: JSON.stringify(result) }], details: result };
  const { image, ...description } = result;
  check(image?.mimeType === 'image/png' && typeof image.data === 'string' && image.data.length <= 42_668 &&
    /^[A-Za-z0-9+/]+={0,2}$/.test(image.data) &&
    createHash('sha256').update(Buffer.from(image.data, 'base64')).digest('hex') === image.sha256,
    'INVALID_VISUAL_RESULT', 'Visual tools must return a bounded verified PNG');
  const { data, ...receipt } = image;
  const visible = { ...description, image: receipt, visualFeedback: supportsImages
    ? { status: 'available', meaning: 'Actual rendered PNG is attached as image content; inspect it before the next edit.' }
    : { status: 'unavailable', reason: 'model_does_not_accept_images', message: 'Visual feedback unavailable: the selected model does not accept images. A PNG preview is available in the project editor; structured summary alone cannot confirm appearance.' } };
  // SDK details are for local renderers. Keep them compact so they do not copy
  // the image or a large structure summary alongside the model content.
  const details = { kind: result.kind, path: result.path, revision: result.revision,
    sha256: result.sha256, image: receipt, visualFeedback: visible.visualFeedback };
  const projected = { content: [{ type: 'text', text: JSON.stringify(visible) },
    ...(supportsImages ? [{ type: 'image', mimeType: 'image/png', data }] : [])], details };
  check(Buffer.byteLength(JSON.stringify(projected)) <= 64_000, 'INVALID_VISUAL_RESULT', 'SDK visual result exceeds 64 KB');
  return projected;
}
export function makePiTools(ctx) {
  return ctx.toolDescriptions.filter(tool => tool.name !== 'build.validate').map(tool => ({
    name: tool.name.replace('.', '__'), label: tool.name, description: tool.description,
    parameters: tool.inputSchema, executionMode: 'sequential',
    async execute(toolCallId, args, signal) {
      throwIfAborted(ctx.signal); throwIfAborted(signal);
      const id = `pi-${createHash('sha256').update(toolCallId).digest('hex')}`;
      const signals = [ctx.signal, signal].filter(Boolean);
      const result = await ctx.invoke(id, tool.name, args, { signal: AbortSignal.any(signals) });
      return projectPiToolResult(tool.name, result, ctx.supportsImages === true);
    },
  }));
}

// Host-side option builder for the next approved integration. Callers MUST supply
// their explicit model/auth runtime and persisted session manager. No defaults.
export function makePiSessionOptions(sdk, ctx, { modelRuntime, model, sessionManager }) {
  check(modelRuntime && model && sessionManager, 'EXPLICIT_SESSION_REQUIRED', 'Supply explicit model/auth runtime, model and session storage');
  const supportsImages = Array.isArray(model.input) && model.input.includes('image');
  const customTools = makePiTools({ ...ctx, supportsImages });
  const settingsManager = sdk.SettingsManager.inMemory({
    compaction: { enabled: false }, retry: { enabled: false, maxRetries: 0, provider: { maxRetries: 0 } },
    cacheWarming: 'off', images: { blockImages: !supportsImages, autoResize: false },
  });
  const agentDir = path.join(ctx.workspaceRoot, '..', 'pi-private');
  const resourceLoader = new sdk.DefaultResourceLoader({
    cwd: ctx.workspaceRoot, agentDir, settingsManager,
    noExtensions: true, noSkills: true, noPromptTemplates: true,
    noThemes: true, noContextFiles: true,
    systemPrompt: ctx.profile.systemPrompt,
    agentsFilesOverride: () => ({ agentsFiles: [] }),
  });
  // No reload/discovery: the SDK loader starts with empty resources. Its constructor
  // does not resolve systemPrompt until reload, so supply the literal host prompt.
  resourceLoader.getSystemPrompt = () => ctx.profile.systemPrompt;
  return { cwd: ctx.workspaceRoot, agentDir, modelRuntime, model, sessionManager,
    settingsManager, resourceLoader, noTools: 'builtin',
    tools: customTools.map(t => t.name), customTools };
}

// Contract-tested with an explicitly injected offline session double only.
// This is not registered as a live provider and does not import/create Pi itself.
export async function runPiSessionContract(ctx, createOfflineSession) {
  throwIfAborted(ctx.signal);
  const { session } = await createOfflineSession({ customTools: makePiTools(ctx), profile: ctx.profile });
  let abortPromise;
  const onAbort = () => { abortPromise ??= Promise.resolve().then(() => session.abort()); };
  const events = [];
  const unsubscribe = session.subscribe(event => {
    // Keep raw prompts, tool arguments, credentials and model text out of logs.
    if (['agent_start', 'agent_settled', 'tool_execution_start', 'tool_execution_end'].includes(event.type)) {
      events.push(ctx.emit(`pi.${event.type}`, {}));
    }
  });
  ctx.signal.addEventListener('abort', onAbort, { once: true });
  try {
    if (ctx.signal.aborted) { onAbort(); await abortPromise; throwIfAborted(ctx.signal); }
    await session.prompt(JSON.stringify(ctx.job.input));
    throwIfAborted(ctx.signal);
    await Promise.all(events);
    return { label: 'OFFLINE_PI_CONTRACT_TEST_ONLY', text: session.getLastAssistantText(), buildValidated: false };
  } finally {
    ctx.signal.removeEventListener('abort', onAbort);
    try { if (abortPromise) await abortPromise; }
    finally { unsubscribe(); session.dispose(); }
  }
}
