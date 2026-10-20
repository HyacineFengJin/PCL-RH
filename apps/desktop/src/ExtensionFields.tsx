import { CeSelect } from "./CeSelect";
import type { ExtensionCard, ExtensionField } from "./experimentalTypes";

// Number drafts keep an empty/intermediate input until explicit submission.
export type ExtensionDraft = Record<string, string | boolean | number>;
// Keep this text boundary aligned with the host's settings value validator.
const unsafeText =
  /[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069\ud800-\udfff]/u;
export function fieldValue(field: ExtensionField, draft: ExtensionDraft) {
  const value = draft[field.id];
  return field.type === "number" && typeof value === "string"
    ? value.trim() === ""
      ? undefined
      : Number(value)
    : value;
}
export function validField(field: ExtensionField, draft: ExtensionDraft) {
  const value = fieldValue(field, draft);
  if (field.type === "text")
    return (
      typeof value === "string" &&
      [...value].length <= field.maxLength &&
      !unsafeText.test(value) &&
      (field.multiline === true || !/[\n\t]/.test(value))
    );
  if (field.type === "toggle") return typeof value === "boolean";
  if (field.type === "select")
    return (
      typeof value === "string" &&
      field.options.some((option) => option.value === value)
    );
  return (
    typeof value === "number" &&
    Number.isInteger(value) &&
    value >= field.min &&
    value <= field.max
  );
}
export function dirtyFields(fields: ExtensionField[], draft: ExtensionDraft) {
  return fields.some((field) => fieldValue(field, draft) !== field.value);
}

export type CachedExtensionDraft = {
  digest: string | undefined;
  fields: ExtensionField[];
  draft: ExtensionDraft;
};
export const extensionDraftKey = (card: ExtensionCard) =>
  `${card.extensionId}:${card.id}`;
function definitionKey(field: ExtensionField) {
  const common = [field.id, field.label, field.type, field.default];
  if (field.type === "text")
    return JSON.stringify([
      ...common,
      field.maxLength,
      field.multiline === true,
    ]);
  if (field.type === "select")
    return JSON.stringify([
      ...common,
      field.options.map((option) => [option.value, option.label]),
    ]);
  if (field.type === "number")
    return JSON.stringify([...common, field.min, field.max]);
  return JSON.stringify(common);
}

/** Refresh actions immediately, retaining only dirty fields from the same
 * verified declaration. A successful read also evicts cards that disappeared. */
export function reconcileExtensionDrafts(
  cards: ExtensionCard[],
  cache: Map<string, CachedExtensionDraft>,
) {
  const next = new Map<string, CachedExtensionDraft>();
  const drafts = new Map<ExtensionCard, ExtensionDraft>();
  for (const card of cards) {
    const key = extensionDraftKey(card),
      previous = cache.get(key),
      fields = card.fields ?? [];
    const draft: ExtensionDraft = Object.fromEntries(
      fields.map((field) => [field.id, field.value]),
    );
    if (card.digest && previous?.digest === card.digest) {
      for (const field of fields) {
        const old = previous.fields.find(
          (candidate) => candidate.id === field.id,
        );
        if (
          old &&
          definitionKey(old) === definitionKey(field) &&
          fieldValue(old, previous.draft) !== old.value
        )
          draft[field.id] = previous.draft[field.id];
      }
    }
    next.set(key, { digest: card.digest, fields, draft });
    drafts.set(card, draft);
  }
  cache.clear();
  for (const [key, value] of next) cache.set(key, value);
  return drafts;
}

/** Rebase committed fields before broadcasting. Later edits remain dirty,
 * while the submitted values become clean when the host projection returns. */
export function markExtensionDraftSaved(
  cache: Map<string, CachedExtensionDraft>,
  card: ExtensionCard,
  values: ExtensionDraft,
) {
  const key = extensionDraftKey(card),
    previous = cache.get(key);
  if (!previous || previous.digest !== card.digest) return;
  cache.set(key, {
    ...previous,
    fields: previous.fields.map((field) =>
      Object.hasOwn(values, field.id)
        ? { ...field, value: values[field.id] }
        : field,
    ),
  });
}

/** All labels/options remain React text. The plugin never supplies a renderer. */
export function ExtensionFields({
  fields,
  draft,
  disabled,
  onChange,
}: {
  fields: ExtensionField[];
  draft: ExtensionDraft;
  disabled: boolean;
  onChange: (id: string, value: string | boolean) => void;
}) {
  return (
    <div className="extension-fields">
      {fields.map((field) => {
        const invalid = !validField(field, draft);
        if (field.type === "toggle")
          return (
            <label key={field.id} className="ce-check extension-toggle">
              <input
                type="checkbox"
                checked={draft[field.id] === true}
                disabled={disabled}
                onChange={(event) => onChange(field.id, event.target.checked)}
              />
              <span>{field.label}</span>
            </label>
          );
        const common = {
          className: "ce-field",
          disabled,
          "aria-label": field.label,
          "aria-invalid": invalid,
        };
        return (
          <label
            key={field.id}
            className={`extension-field${field.type === "text" && field.multiline ? " extension-field-multiline" : ""}`}
          >
            <span>{field.label}</span>
            {field.type === "select" ? (
              <CeSelect
                {...common}
                value={String(draft[field.id])}
                onChange={(event) => onChange(field.id, event.target.value)}
              >
                {field.options.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </CeSelect>
            ) : field.type === "text" && field.multiline ? (
              <textarea
                {...common}
                rows={4}
                maxLength={field.maxLength * 2}
                value={String(draft[field.id])}
                onChange={(event) => onChange(field.id, event.target.value)}
              />
            ) : (
              <input
                {...common}
                type={field.type === "number" ? "number" : "text"}
                value={String(draft[field.id])}
                maxLength={
                  field.type === "text" ? field.maxLength * 2 : undefined
                }
                min={field.type === "number" ? field.min : undefined}
                max={field.type === "number" ? field.max : undefined}
                step={field.type === "number" ? 1 : undefined}
                onChange={(event) => onChange(field.id, event.target.value)}
              />
            )}
          </label>
        );
      })}
    </div>
  );
}
