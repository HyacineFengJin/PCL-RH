/** Editable work budgets are separate from transport/receipt size safeguards.
 * Zero removes a work budget; the ranges describe supported positive values. */
export const PI_WORK_LIMITS = Object.freeze({
  modelTurns: 32,
  toolCalls: 128,
  maximumOutputTokens: 8192,
  wallTimeMs: 900_000,
  estimatedBudgetUsd: 5,
});
export const PI_LIMIT_RANGES = Object.freeze({
  modelTurns: [1, 256],
  toolCalls: [1, 200],
  maximumOutputTokens: [16, 131072],
  wallTimeMs: [10_000, 86_400_000],
  estimatedBudgetUsd: [0.01, 10000],
});
export const PI_TRANSPORT_LIMITS = Object.freeze({
  requestBytes: 1_000_000,
  responseBytes: 4_000_000,
  resultBytes: 48_000,
});
export function normalizeWorkLimits(input = {}) {
  if (
    !input ||
    typeof input !== "object" ||
    Array.isArray(input) ||
    Object.keys(input).some((key) => !Object.hasOwn(PI_WORK_LIMITS, key))
  )
    throw new Error("Unknown AI work limit");
  const result = { ...PI_WORK_LIMITS, ...input };
  for (const [key, value] of Object.entries(result)) {
    const [min, max] = PI_LIMIT_RANGES[key];
    if (
      !Number.isFinite(value) ||
      (value !== 0 && value < min) ||
      value > max ||
      (key !== "estimatedBudgetUsd" && !Number.isInteger(value))
    )
      throw new Error("AI work limit is outside its supported range");
  }
  return Object.freeze(result);
}
