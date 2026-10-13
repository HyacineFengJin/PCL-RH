import { check, RuntimeError, throwIfAborted } from "../errors.mjs";
import { makePiSessionOptions, PINNED_PI } from "./pi.mjs";
import {
  createPrivateModelRuntime,
  installPiModel,
  validatePiRequest,
} from "./pi-selection.mjs";
import {
  PI_WORK_LIMITS,
  PI_TRANSPORT_LIMITS,
  PI_LIMIT_RANGES,
} from "./pi-limits.mjs";

export const LIVE_PI_LIMITS = Object.freeze({
  ...PI_WORK_LIMITS,
  ...PI_TRANSPORT_LIMITS,
});
function runLimits(overrides) {
  check(
    overrides &&
      typeof overrides === "object" &&
      !Array.isArray(overrides) &&
      Object.keys(overrides).every((key) => Object.hasOwn(LIVE_PI_LIMITS, key)),
    "INVALID_LIVE_CONFIG",
    "Unknown run limit",
  );
  const limits = { ...LIVE_PI_LIMITS, ...overrides };
  for (const [key, value] of Object.entries(limits)) {
    check(
      Number.isFinite(value) &&
        (value > 0 || (value === 0 && Object.hasOwn(PI_WORK_LIMITS, key))) &&
        value <= (PI_LIMIT_RANGES[key]?.[1] ?? PI_TRANSPORT_LIMITS[key]) &&
        (key === "estimatedBudgetUsd" || Number.isInteger(value)),
      "INVALID_LIVE_CONFIG",
      "Invalid host run limit",
    );
  }
  return Object.freeze(limits);
}
const SAFE_CODES = new Set([
  "LIVE_MODEL_CALLS_DISABLED",
  "LIVE_RESUME_UNSUPPORTED",
  "INVALID_LIVE_CONFIG",
  "SDK_VERSION_MISMATCH",
  "MODEL_UNAVAILABLE",
  "CREDENTIAL_REQUIRED",
  "MODEL_TURN_LIMIT",
  "MODEL_TOOL_LIMIT",
  "MODEL_REQUEST_LIMIT",
  "MODEL_RESPONSE_LIMIT",
  "MODEL_RESULT_LIMIT",
  "MODEL_SPEND_LIMIT",
  "MODEL_TIMEOUT",
  "MODEL_REQUEST_DENIED",
  "MODEL_FAILED",
  "MODEL_OUTPUT_LIMIT",
  "CANCELLED",
  "PROVIDER_CONFIG_DENIED",
]);
function containsSecret(value, secret) {
  if (typeof value === "string") return value.includes(secret);
  if (!value || typeof value !== "object") return false;
  return Object.entries(value).some(
    ([name, child]) => name.includes(secret) || containsSecret(child, secret),
  );
}
// Streamed text is a display projection, not a durable transcript. Redact a
// complete key and withhold a trailing key prefix until the next delta arrives,
// otherwise a credential split across chunks could be exposed temporarily.
function publicAssistantOutput(text, secret) {
  let output = secret ? text.split(secret).join("[redacted]") : text;
  if (secret) {
    for (let length = Math.min(secret.length - 1, output.length); length > 0; length--) {
      if (output.endsWith(secret.slice(0, length))) {
        output = output.slice(0, -length);
        break;
      }
    }
  }
  output = output.slice(0, 16_000);
  // Escaped control characters can make a short string exceed the checkpoint's
  // JSON budget. Keep enough space for counters and the rest of provider state.
  if (Buffer.byteLength(JSON.stringify(output)) > 48_000) {
    let low = 0, high = output.length;
    while (low < high) {
      const length = Math.ceil((low + high) / 2);
      if (Buffer.byteLength(JSON.stringify(output.slice(0, length))) <= 48_000) low = length;
      else high = length - 1;
    }
    output = output.slice(0, low);
  }
  return output;
}

/** Real SDK integration. The host owns activation, provider selection and limits, never job input. */
export class LivePiProvider {
  id = "pi-live";
  #enabled;
  #getSelection;
  #limits;
  #getLimits;
  #loadSdk;
  #fetch;
  constructor(
    { enabled = false, getSelection, getLimits, limits = {} } = {},
    dependencies = {},
  ) {
    check(
      typeof enabled === "boolean" &&
        (getSelection === undefined || typeof getSelection === "function") &&
        (getLimits === undefined || typeof getLimits === "function"),
      "INVALID_LIVE_CONFIG",
      "Use a host enable flag and a host-owned selection callback",
    );
    this.#limits = runLimits(limits);
    this.#getLimits = getLimits;
    this.#enabled = enabled;
    this.#getSelection = getSelection;
    this.#loadSdk =
      dependencies.loadSdk ?? (() => import("@earendil-works/pi-coding-agent"));
    // This injection is for host-owned mock transports; no job-config override exists.
    this.#fetch =
      dependencies.fetch ?? ((...args) => globalThis.fetch(...args));
  }
  // The durable runtime must use the same host-owned count budget. Otherwise
  // its legacy 200-operation cap would defeat an unlimited preset.
  getToolOperationLimit(job) {
    return this.#getLimits
      ? runLimits(this.#getLimits(job)).toolCalls
      : this.#limits.toolCalls;
  }
  async run(ctx) {
    check(
      this.#enabled,
      "LIVE_MODEL_CALLS_DISABLED",
      "Explicit host enablement is required",
    );
    check(
      ctx.job.attempt === 1,
      "LIVE_RESUME_UNSUPPORTED",
      "Live jobs are single-attempt; review receipts before explicitly starting a new job",
    );
    check(
      Object.keys(ctx.job.providerConfig ?? {}).length === 0,
      "PROVIDER_CONFIG_DENIED",
      "Job input cannot configure provider access",
    );
    check(
      this.#getSelection,
      "CREDENTIAL_REQUIRED",
      "Configure a provider through the host entry flow",
    );
    // Capture the route and key together before any asynchronous SDK load.
    const selection = this.#getSelection(ctx.job);
    check(
      selection,
      "CREDENTIAL_REQUIRED",
      "Configure a session provider first",
    );
    throwIfAborted(ctx.signal);
    const limits = this.#getLimits
      ? runLimits(this.#getLimits(ctx.job))
      : this.#limits;
    const controller = new AbortController();
    const signal = AbortSignal.any([ctx.signal, controller.signal]);
    let session,
      unsubscribe = () => {},
      abortPromise,
      key,
      failure,
      awaitingUser;
    const activity = { phase: "starting", textChars: 0 };
    let activityWrites = Promise.resolve();
    let lastActivitySave = 0;
    let outputTimer;
    let assistantOutput = "";
    let output = "";
    const counts = {
      modelTurns: 0,
      requests: 0,
      toolCalls: 0,
      reservedEstimatedUsd: selection.costKnown ? 0 : null,
      priceEstimateAvailable: selection.costKnown,
    };
    const stop = (code, message) => {
      failure ??= new RuntimeError(code, message);
      controller.abort();
      return failure;
    };
    const timer =
      limits.wallTimeMs > 0
        ? setTimeout(
            () => stop("MODEL_TIMEOUT", "The live job reached its time limit"),
            limits.wallTimeMs,
          )
        : undefined;
    const onAbort = () => {
      if (session)
        abortPromise ??= Promise.resolve()
          .then(() => session.abort())
          .catch(() => {});
    };
    signal.addEventListener("abort", onAbort);
    try {
      const sdk = await this.#loadSdk();
      check(
        sdk.VERSION === PINNED_PI.version,
        "SDK_VERSION_MISMATCH",
        "Installed Pi differs from the pinned version",
      );
      throwIfAborted(signal);
      // A dummy credential is only for user-selected unauthenticated local/custom
      // services. It is never treated as a secret or claimed to be a saved key.
      key = selection.key || "pcl-local-no-auth";
      const credentials = {
        read: async (id) =>
          id === selection.provider ? { type: "api_key", key } : undefined,
        list: async () => [{ providerId: selection.provider, type: "api_key" }],
        modify: async () => {
          throw new Error("Credential writes are disabled");
        },
        delete: async () => {
          throw new Error("Credential writes are disabled");
        },
      };
      const modelRuntime = await createPrivateModelRuntime(
        sdk,
        credentials,
        signal,
      );
      const selectedModel = installPiModel(modelRuntime, selection);
      // Providers require a positive maxTokens. Zero removes our ceiling and
      // uses the selected model's capacity; it must never be sent as maxTokens: 0.
      const outputLimit =
        limits.maximumOutputTokens === 0
          ? selectedModel.maxTokens
          : Math.min(limits.maximumOutputTokens, selectedModel.maxTokens);
      // Pi can add a thinking budget to maxTokens for Anthropic. Clamp the
      // per-job model ceiling as well, so that addition stays under our cap.
      const model = { ...selectedModel, maxTokens: outputLimit };
      const saveCounts = () =>
        ctx.checkpoint({
          livePi: { schemaVersion: 2, ...counts, output, activity: { ...activity }, liveResumeSupported: false },
        });
      // SDK subscription callbacks are synchronous. Queue small host-owned
      // activity projections and drain them before finalizing the round. Raw
      // deltas, tool arguments and provider diagnostics never enter the log.
      const saveActivity = (event, metadata = {}) => {
        const livePi = { schemaVersion: 2, ...counts, output, activity: { ...activity }, liveResumeSupported: false };
        clearTimeout(outputTimer); outputTimer = undefined;
        lastActivitySave = Date.now();
        activityWrites = activityWrites.then(async () => {
          if (signal.aborted) return;
          await ctx.checkpoint({ livePi });
          if (event) await ctx.emit(event, metadata);
        }).catch(() => {
          if (!signal.aborted) stop("MODEL_FAILED", "Live activity could not be persisted");
        });
      };
      const guardFetch = async (url, options = {}) => {
        throwIfAborted(signal);
        const bytes = Buffer.byteLength(options.body);
        if (bytes > limits.requestBytes)
          throw stop(
            "MODEL_REQUEST_LIMIT",
            "The model request exceeded its size limit",
          );
        validatePiRequest(model, url, options, outputLimit);
        if (limits.modelTurns > 0 && counts.requests >= limits.modelTurns)
          throw stop("MODEL_TURN_LIMIT", "The job reached its request limit");
        if (selection.costKnown) {
          // Cache-miss estimate with input overhead, not a provider billing quote.
          const estimate =
            ((2 * bytes + 4096) * selection.cost.input +
              outputLimit * selection.cost.output) /
            1_000_000;
          if (
            limits.estimatedBudgetUsd > 0 &&
            counts.reservedEstimatedUsd + estimate > limits.estimatedBudgetUsd
          )
            throw stop(
              "MODEL_SPEND_LIMIT",
              "The next request exceeds the local estimated spend allowance",
            );
          counts.reservedEstimatedUsd += estimate;
        }
        counts.requests++;
        await saveCounts(); // Reserve before sending; an uncertain/failed request is not refunded.
        throwIfAborted(signal);
        const response = await this.#fetch(url, {
          ...options,
          redirect: "error",
          signal: AbortSignal.any([signal, options.signal].filter(Boolean)),
        });
        if (!response.body) return response;
        const reader = response.body.getReader();
        let readBytes = 0;
        const body = new ReadableStream({
          async pull(target) {
            try {
              const part = await reader.read();
              if (part.done) {
                target.close();
                return;
              }
              readBytes += part.value.byteLength;
              if (readBytes > limits.responseBytes) {
                const error = stop(
                  "MODEL_RESPONSE_LIMIT",
                  "The provider response exceeded its size limit",
                );
                await reader.cancel();
                target.error(error);
                return;
              }
              target.enqueue(part.value);
            } catch (error) {
              target.error(error);
            }
          },
          cancel: (reason) => reader.cancel(reason),
        });
        return new Response(body, {
          status: response.status,
          statusText: response.statusText,
          headers: response.headers,
        });
      };
      const toolContext = {
        ...ctx,
        signal,
        supportsImages: Array.isArray(model.input) && model.input.includes("image"),
        invoke: async (...args) => {
          throwIfAborted(signal);
          counts.toolCalls++;
          if (limits.toolCalls > 0 && counts.toolCalls > limits.toolCalls)
            throw stop(
              "MODEL_TOOL_LIMIT",
              "The job reached its tool-call limit",
            );
          if (selection.key && containsSecret(args[2], selection.key))
            throw stop(
              "MODEL_REQUEST_DENIED",
              "Credential-like tool input was refused",
            );
          await saveCounts();
          const result = await ctx.invoke(...args);
          if (
            ctx.job.profile === "porter" &&
            args[1] === "porter.ask_user" &&
            result.status === "awaiting_user"
          ) {
            // A host-persisted question is a successful round boundary. Abort
            // the model loop so waiting for a human uses no provider calls or
            // runtime admission. Only this registered tool can set the state.
            awaitingUser = result;
            controller.abort();
          }
          return result;
        },
      };
      const options = makePiSessionOptions(sdk, toolContext, {
        modelRuntime,
        model,
        sessionManager: sdk.SessionManager.inMemory(ctx.workspaceRoot),
      });
      ({ session } = await sdk.createAgentSession({
        ...options,
        thinkingLevel: selection.thinking,
      }));
      // Use the real Pi session/tool loop and real provider serializer. The guarded
      // transport is the only request path: no warming, compaction, fallback or retries.
      session.agent.streamFunction = async (
        requestModel,
        context,
        requestOptions = {},
      ) => {
        throwIfAborted(signal);
        counts.modelTurns++;
        if (limits.modelTurns > 0 && counts.modelTurns > limits.modelTurns)
          throw stop(
            "MODEL_TURN_LIMIT",
            "The job reached its model-turn limit",
          );
        check(
          requestModel.id === model.id &&
            requestModel.provider === model.provider &&
            requestModel.baseUrl === model.baseUrl,
          "MODEL_REQUEST_DENIED",
          "Model routing changes are disabled",
        );
        activity.phase = "requesting";
        delete activity.toolName;
        saveActivity("pi.model.turn.started", { turn: counts.modelTurns });
        await activityWrites;
        throwIfAborted(signal);
        return modelRuntime.streamSimple(model, context, {
          ...requestOptions,
          apiKey: key,
          maxTokens: outputLimit,
          maxRetries: 0,
          maxRetryDelayMs: 0,
          timeoutMs: limits.wallTimeMs || undefined,
          cacheRetention: "none",
          fetch: guardFetch,
          env: {},
          signal: AbortSignal.any(
            [signal, requestOptions.signal].filter(Boolean),
          ),
        });
      };
      const toolNames = new Map(ctx.toolDescriptions.map((tool) => [tool.name.replace(".", "__"), tool.name]));
      unsubscribe = session.subscribe((event) => {
        if (event.type === "message_update") {
          const delta = event.assistantMessageEvent;
          if (delta?.type === "text_delta") {
            const firstText = activity.phase !== "responding";
            activity.phase = "responding";
            delete activity.toolName;
            activity.textChars += typeof delta.delta === "string" ? delta.delta.length : 0;
            // Retain one bounded prefix of visible text, including enough look
            // ahead to recognize a key that crosses the display limit.
            assistantOutput = (assistantOutput + (typeof delta.delta === "string" ? delta.delta : ""))
              .slice(0, 16_000 + (selection.key?.length ?? 0));
            output = publicAssistantOutput(assistantOutput, selection.key);
            if (firstText || Date.now() - lastActivitySave >= 250) saveActivity();
            else outputTimer ??= setTimeout(() => {
              outputTimer = undefined;
              if (!signal.aborted) saveActivity();
            }, Math.max(1, 250 - (Date.now() - lastActivitySave)));
          }
        } else if (["tool_execution_start", "tool_execution_end"].includes(event.type)) {
          const name = toolNames.get(event.toolName);
          activity.phase = event.type === "tool_execution_start" ? "tool" : "requesting";
          if (name && activity.phase === "tool") activity.toolName = name;
          else delete activity.toolName;
          if (name) saveActivity(event.type === "tool_execution_start" ? "pi.tool.started" : "pi.tool.completed",
            { name, ...(event.type === "tool_execution_end" ? { failed: !!event.isError } : {}) });
        } else if (event.type === "message_end" && event.message.role === "assistant") {
          saveActivity("pi.model.turn.completed", { turn: counts.modelTurns });
        }
        if (
          !awaitingUser &&
          event.type === "message_end" &&
          event.message.role === "assistant" &&
          ["error", "aborted"].includes(event.message.stopReason)
        ) {
          stop(
            "MODEL_FAILED",
            "The provider request failed; private provider details were withheld",
          );
        }
      });
      if (signal.aborted) onAbort();
      throwIfAborted(signal);
      await ctx.emit("pi.live.started", {
        provider: selection.provider,
        model: model.id,
      });
      await session.prompt(JSON.stringify(ctx.job.input), {
        expandPromptTemplates: false,
      });
      await activityWrites;
      if (failure) throw failure;
      throwIfAborted(signal);
      const last = session.messages
        .filter((message) => message.role === "assistant")
        .at(-1);
      check(
        last?.stopReason === "stop",
        "MODEL_OUTPUT_LIMIT",
        "The model did not produce a complete final response",
      );
      const text = session.getLastAssistantText() ?? "";
      check(
        Buffer.byteLength(text) <= limits.resultBytes &&
          (!selection.key || !text.includes(selection.key)),
        "MODEL_RESULT_LIMIT",
        "Final model text exceeded the safe result limit",
      );
      activity.phase = "idle";
      delete activity.toolName;
      await saveCounts();
      return {
        label: "UNTRUSTED_MODEL_PROPOSAL",
        text,
        provider: selection.provider,
        model: model.id,
        ...counts,
        buildValidated: false,
        runtimeValidated: false,
        approvalGranted: false,
      };
    } catch (error) {
      if (ctx.signal.aborted)
        throw new RuntimeError("CANCELLED", "Job was cancelled");
      if (failure) throw failure;
      if (awaitingUser)
        return {
          label: "PORTER_AWAITING_USER",
          ...awaitingUser,
          ...counts,
          buildValidated: false,
          runtimeValidated: false,
          approvalGranted: false,
        };
      if (error instanceof RuntimeError && SAFE_CODES.has(error.code))
        throw error;
      throw new RuntimeError(
        "MODEL_FAILED",
        "The live provider could not finish; private provider details were withheld",
      );
    } finally {
      clearTimeout(timer);
      clearTimeout(outputTimer);
      signal.removeEventListener("abort", onAbort);
      try {
        if (abortPromise) await abortPromise;
        await activityWrites;
      } finally {
        unsubscribe();
        session?.dispose();
        assistantOutput = "";
        key = undefined;
      }
    }
  }
}
