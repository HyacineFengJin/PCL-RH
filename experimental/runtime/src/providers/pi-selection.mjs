/** Host-owned Pi routing. Catalog reads are offline; only the private preset
 * store retains keys. Job input cannot inject credentials, commands or routes. */
import { check } from "../errors.mjs";
import { PINNED_PI } from "./pi.mjs";

export const CUSTOM_PROVIDER = "pcl-custom";
export const CUSTOM_APIS = Object.freeze([
  "openai-completions",
  "openai-responses",
  "anthropic-messages",
]);
const GUARDED_APIS = new Set([...CUSTOM_APIS, "mistral-conversations"]);
// These providers have ordinary API-key flows. OAuth/cloud account discovery is
// deliberately not inherited from the user's Pi CLI authentication directory.
const BUILTINS = new Set([
  "openai",
  "anthropic",
  "deepseek",
  "openrouter",
  "groq",
  "mistral",
  "xai",
  "moonshotai",
  "moonshotai-cn",
  "kimi-coding",
  "minimax",
  "minimax-cn",
  "cerebras",
  "together",
  "fireworks",
  "huggingface",
  "nvidia",
  "zai",
  "zai-coding-cn",
  "baseten",
  "opencode",
  "opencode-go",
  "ant-ling",
  "xiaomi",
  "qwen-token-plan",
  "qwen-token-plan-cn",
]);
export const THINKING_LEVELS = Object.freeze([
  "off",
  "minimal",
  "low",
  "medium",
  "high",
]);
const emptyCredentials = {
  read: async () => undefined,
  list: async () => [],
  modify: async () => {
    throw new Error("Credential writes are disabled");
  },
  delete: async () => {
    throw new Error("Credential writes are disabled");
  },
};
export async function createPrivateModelRuntime(
  sdk,
  credentials = emptyCredentials,
  signal,
) {
  check(
    sdk.VERSION === PINNED_PI.version,
    "SDK_VERSION_MISMATCH",
    "Installed Pi differs from the pinned version",
  );
  return sdk.ModelRuntime.create({
    credentials,
    modelsPath: null,
    allowModelNetwork: false,
    refreshOnCreate: false,
    signal,
  });
}
function baseUrl(value) {
  check(
    typeof value === "string" &&
      value.length <= 2048 &&
      !/[\s\x00-\x1f\x7f]/.test(value),
    "INVALID_LIVE_CONFIG",
    "Provide a valid API base URL",
  );
  let url;
  try {
    url = new URL(value);
  } catch {
    check(false, "INVALID_LIVE_CONFIG", "Provide an absolute API base URL");
  }
  check(
    ["https:", "http:"].includes(url.protocol) &&
      !url.username &&
      !url.password &&
      !url.search &&
      !url.hash &&
      !/[{}]/.test(value),
    "INVALID_LIVE_CONFIG",
    "API base URL cannot contain credentials, query parameters or placeholders",
  );
  return url.href.replace(/\/+$/, "");
}
function hasPrice(cost) {
  return (
    !!cost &&
    ["input", "output"].every((k) => Number.isFinite(cost[k]) && cost[k] >= 0)
  );
}
let catalogPromise;
export function getPiCatalog() {
  // A pinned bundled snapshot, not a network refresh or credential inspection.
  catalogPromise ??= (async () => {
    const sdk = await import("@earendil-works/pi-coding-agent");
    const runtime = await createPrivateModelRuntime(sdk);
    return {
      providers: runtime
        .getProviders()
        .filter((p) => BUILTINS.has(p.id))
        .map((provider) => ({
          id: provider.id,
          name: provider.name ?? provider.id,
          models: runtime
            .getModels(provider.id)
            .filter((model) => {
              if (
                !GUARDED_APIS.has(model.api) ||
                !model.input.includes("text") ||
                (model.api === "openai-responses" &&
                  model.compat?.supportsMaxOutputTokens === false) ||
                model.maxTokens < 16
              )
                return false;
              try {
                baseUrl(model.baseUrl);
                return true;
              } catch {
                return false;
              }
            })
            .map((model) => ({
              id: model.id,
              name: model.name,
              api: model.api,
              baseUrl: model.baseUrl,
              reasoning: model.reasoning,
              contextWindow: model.contextWindow,
              maxTokens: model.maxTokens,
              supportsImages: model.input.includes("image"),
              cost: hasPrice(model.cost)
                ? { input: model.cost.input, output: model.cost.output }
                : null,
            })),
        }))
        .filter((provider) => provider.models.length),
    };
  })().catch((error) => {
    catalogPromise = undefined;
    throw error;
  });
  return catalogPromise;
}
function boundedText(value, limit, message) {
  check(
    typeof value === "string" &&
      value.length > 0 &&
      value.length <= limit &&
      !/[\x00-\x1f\x7f]/.test(value),
    "INVALID_LIVE_CONFIG",
    message,
  );
  return value;
}
export async function normalizePiSelection(input) {
  check(
    input &&
      typeof input === "object" &&
      !Array.isArray(input) &&
      Object.keys(input).every((k) =>
        [
          "provider",
          "model",
          "key",
          "baseUrl",
          "api",
          "contextWindow",
          "cost",
          "thinking",
          "supportsImages",
        ].includes(k),
      ),
    "INVALID_LIVE_CONFIG",
    "Unknown provider configuration",
  );
  const thinking = input.thinking ?? "off";
  check(
    input.supportsImages === undefined ||
      typeof input.supportsImages === "boolean",
    "INVALID_LIVE_CONFIG",
    "Image capability must be a trusted boolean declaration",
  );
  check(
    THINKING_LEVELS.includes(thinking),
    "INVALID_LIVE_CONFIG",
    "Choose a supported thinking level",
  );
  const key = input.key ?? "";
  check(
    typeof key === "string" &&
      key.length <= 2048 &&
      !/[\s\x00-\x1f\x7f]/.test(key),
    "CREDENTIAL_REQUIRED",
    "Provide an API key without whitespace",
  );
  const model = boundedText(input.model, 256, "Provide a model ID");
  if (input.provider !== CUSTOM_PROVIDER) {
    const catalog = await getPiCatalog();
    const selected = catalog.providers
      .find((p) => p.id === input.provider)
      ?.models.find((m) => m.id === model);
    check(
      selected,
      "MODEL_UNAVAILABLE",
      "Choose a model from the bundled Pi catalog",
    );
    check(
      key.length > 0,
      "CREDENTIAL_REQUIRED",
      "An API key is required for this provider",
    );
    check(
      input.provider !== "openai" || key.startsWith("sk-"),
      "CREDENTIAL_REQUIRED",
      "Use an OpenAI API key, not an OAuth token",
    );
    check(
      input.baseUrl === undefined &&
        input.api === undefined &&
        input.contextWindow === undefined &&
        input.cost === undefined &&
        input.supportsImages === undefined,
      "INVALID_LIVE_CONFIG",
      "Use a custom provider to override model routing",
    );
    return Object.freeze({
      ...selected,
      provider: input.provider,
      key,
      thinking,
      custom: false,
      costKnown: hasPrice(selected.cost),
    });
  }
  check(
    CUSTOM_APIS.includes(input.api),
    "INVALID_LIVE_CONFIG",
    "Choose a supported custom API protocol",
  );
  const contextWindow = input.contextWindow ?? 32768;
  check(
    Number.isInteger(contextWindow) &&
      contextWindow >= 4096 &&
      contextWindow <= 2_000_000,
    "INVALID_LIVE_CONFIG",
    "Context window must be between 4096 and 2000000 tokens",
  );
  check(
    input.cost == null ||
      (Object.keys(input.cost).every((k) => ["input", "output"].includes(k)) &&
        hasPrice(input.cost) &&
        input.cost.input <= 10000 &&
        input.cost.output <= 10000),
    "INVALID_LIVE_CONFIG",
    "Prices must be nonnegative USD per million tokens",
  );
  return Object.freeze({
    provider: CUSTOM_PROVIDER,
    id: model,
    name: model,
    api: input.api,
    baseUrl: baseUrl(input.baseUrl),
    contextWindow,
    maxTokens: Math.min(contextWindow, 131072),
    reasoning: false,
    thinking,
    key,
    custom: true,
    supportsImages: input.supportsImages ?? false,
    costKnown: hasPrice(input.cost),
    cost: input.cost ? Object.freeze({ ...input.cost }) : null,
  });
}
export function publicPiSelection(selection) {
  if (!selection) return null;
  // Return metadata only, never serialize a stored credential into status.
  return {
    provider: selection.provider,
    model: selection.id,
    api: selection.api,
    baseUrl: selection.baseUrl,
    contextWindow: selection.contextWindow,
    thinking: selection.thinking,
    cost: selection.cost,
    costKnown: selection.costKnown,
    keyConfigured: selection.key.length > 0,
    supportsImages: selection.supportsImages === true,
  };
}
export function installPiModel(runtime, selection) {
  if (selection.custom) {
    runtime.registerProvider(CUSTOM_PROVIDER, {
      name: "Custom provider",
      baseUrl: selection.baseUrl,
      api: selection.api,
      models: [
        {
          id: selection.id,
          name: selection.name,
          api: selection.api,
          input:
            selection.supportsImages === true ? ["text", "image"] : ["text"],
          reasoning: false,
          cost: {
            input: 0,
            output: 0,
            cacheRead: 0,
            cacheWrite: 0,
            ...(selection.cost ?? {}),
          },
          contextWindow: selection.contextWindow,
          maxTokens: selection.maxTokens,
          compat:
            selection.api === "openai-completions"
              ? { supportsStrictMode: false, maxTokensField: "max_tokens" }
              : undefined,
        },
      ],
    });
  }
  const model = runtime.getModel(selection.provider, selection.id);
  check(
    model &&
      GUARDED_APIS.has(model.api) &&
      model.baseUrl.replace(/\/+$/, "") ===
        selection.baseUrl.replace(/\/+$/, ""),
    "MODEL_UNAVAILABLE",
    "The selected Pi model is unavailable",
  );
  return model.api === "openai-completions"
    ? { ...model, compat: { ...model.compat, supportsStrictMode: false } }
    : model;
}
export function selectedRequestEndpoint(model) {
  const suffix =
    model.api === "anthropic-messages"
      ? "v1/messages"
      : model.api === "openai-responses"
        ? "responses"
        : model.api === "mistral-conversations"
          ? "v1/chat/completions"
          : "chat/completions";
  return `${baseUrl(model.baseUrl)}/${suffix}`;
}
export function validatePiRequest(model, url, options, outputLimit) {
  const target = String(url);
  const endpoint = selectedRequestEndpoint(model);
  // The pinned Anthropic SDK adds the fixed beta switch to its message path.
  const routeMatches =
    target === endpoint ||
    (model.api === "anthropic-messages" && target === `${endpoint}?beta=true`);
  check(
    (typeof url === "string" || url instanceof URL) &&
      routeMatches &&
      options.method === "POST" &&
      typeof options.body === "string",
    "MODEL_REQUEST_DENIED",
    "Only the selected model endpoint is allowed",
  );
  let payload;
  try {
    payload = JSON.parse(options.body);
  } catch {
    check(false, "MODEL_REQUEST_DENIED", "Invalid provider request");
  }
  const maximum =
    model.api === "openai-responses"
      ? payload.max_output_tokens
      : (payload.max_tokens ?? payload.max_completion_tokens);
  check(
    payload.model === model.id &&
      Number.isInteger(maximum) &&
      maximum > 0 &&
      maximum <= outputLimit &&
      payload.stream === true,
    "MODEL_REQUEST_DENIED",
    "The provider request changed its fixed model or limits",
  );
  return payload;
}
