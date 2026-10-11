/** Portable presets contain only workspace geometry and view identities.
 * Validate the original tree before dropping private bindings so malformed
 * state cannot bypass the same bounds enforced by the native project store. */
import type { Layout, ViewId } from "./types";

const MAX_BYTES = 256 * 1024;
const MAX_STATE_BYTES = 64 * 1024;
const unsafeKeys = new Set(["__proto__", "constructor", "prototype"]);
const safeId = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const safeKey = /^[A-Za-z0-9._-]+$/;
const controls = /[\u0000-\u001f\u007f-\u009f]/;
const encoder = new TextEncoder();
type ObjectValue = Record<string, unknown>;

function fail(message: string): never {
  throw new Error(message);
}
function bytes(text: string): number {
  return encoder.encode(text).length;
}
function object(value: unknown): ObjectValue {
  if (
    value === null ||
    typeof value !== "object" ||
    Array.isArray(value) ||
    ![Object.prototype, null].includes(Object.getPrototypeOf(value))
  )
    fail("Expected a JSON object");
  const result = value as ObjectValue;
  if (Object.getOwnPropertySymbols(result).length)
    fail("Unsupported JSON property");
  for (const [key, property] of Object.entries(
    Object.getOwnPropertyDescriptors(result),
  )) {
    if (unsafeKeys.has(key)) fail("Unsafe JSON key");
    if (!property.enumerable || !("value" in property))
      fail("Unsupported JSON property");
  }
  return result;
}
function fields(value: ObjectValue, allowed: string[]): void {
  if (Object.keys(value).some((key) => !allowed.includes(key)))
    fail("Unsupported layout field");
}
function identifier(value: unknown, limit: number, label: string): string {
  if (typeof value !== "string" || value.length > limit || !safeId.test(value))
    fail(`Invalid ${label}`);
  return value;
}
function nameValue(value: unknown): string {
  if (typeof value !== "string") fail("Invalid preset name");
  const name = value.trim();
  if (!name || [...name].length > 80 || controls.test(name))
    fail("Invalid preset name");
  return name;
}

/** Count exact JSON bytes while visiting state, stopping before unbounded
 * strings/collections can reach JSON.stringify or recursive traversal. */
function validateState(value: unknown): void {
  const state = object(value);
  let total = 0;
  const add = (amount: number) => {
    total += amount;
    if (total > MAX_STATE_BYTES) fail("Layout state exceeds 64 KiB");
  };
  const visit = (item: unknown, depth: number): void => {
    if (depth > 8) fail("Layout state is too deep");
    if (item === null || typeof item === "boolean") {
      add(item === null ? 4 : item ? 4 : 5);
    } else if (typeof item === "string") {
      if (bytes(item) > MAX_STATE_BYTES) fail("Layout state exceeds 64 KiB");
      add(bytes(JSON.stringify(item)));
    } else if (typeof item === "number") {
      if (!Number.isFinite(item)) fail("Invalid JSON number");
      add(JSON.stringify(item).length);
    } else if (Array.isArray(item)) {
      if (
        Object.getPrototypeOf(item) !== Array.prototype ||
        Object.getOwnPropertySymbols(item).length ||
        Object.keys(item).length !== item.length
      )
        fail("Invalid JSON array");
      add(2 + Math.max(0, item.length - 1));
      for (let index = 0; index < item.length; index++) {
        const property = Object.getOwnPropertyDescriptor(item, String(index));
        if (!property || !property.enumerable || !("value" in property))
          fail("Invalid JSON array");
        visit(property.value, depth + 1);
      }
    } else {
      const record = object(item);
      const keys = Object.keys(record);
      add(2 + Math.max(0, keys.length - 1));
      for (const key of keys) {
        if (key.length > 120 || !safeKey.test(key)) fail("Invalid state key");
        add(bytes(JSON.stringify(key)) + 1);
        visit(record[key], depth + 1);
      }
    }
  };
  for (const [view, data] of Object.entries(state)) {
    identifier(view, 120, "state view ID");
    object(data);
  }
  visit(state, 0);
}

function validateLayout(input: unknown): Layout {
  const ids = new Set<string>();
  let areas = 0;
  const visit = (inputNode: unknown, depth: number): void => {
    if (depth > 7) fail("Layout is too deep");
    const node = object(inputNode);
    const id = identifier(node.id, 100, "node ID");
    if (ids.has(id)) fail("Duplicate layout node ID");
    ids.add(id);
    if (node.type === "area") {
      fields(node, ["type", "id", "view", "conversation", "file", "state"]);
      if (++areas > 12) fail("Layout has more than 12 areas");
      identifier(node.view, 120, "view ID");
      if (node.conversation !== undefined)
        identifier(node.conversation, 100, "conversation ID");
      if (node.file !== undefined) {
        const file = node.file;
        if (
          typeof file !== "string" ||
          !file.trim() ||
          bytes(file) > 4096 ||
          controls.test(file) ||
          file.includes("\\") ||
          file.includes(":") ||
          file.split("/").some((part) => !part || part.startsWith("."))
        )
          fail("Invalid layout file path");
      }
      if (node.state !== undefined) validateState(node.state);
    } else if (node.type === "split") {
      fields(node, ["type", "id", "axis", "ratio", "children"]);
      if (node.axis !== "row" && node.axis !== "column")
        fail("Invalid split axis");
      if (
        typeof node.ratio !== "number" ||
        !Number.isFinite(node.ratio) ||
        node.ratio < 0.15 ||
        node.ratio > 0.85
      )
        fail("Invalid split ratio");
      if (!Array.isArray(node.children) || node.children.length !== 2)
        fail("A split must contain two children");
      // JSON arrays carry only indexed elements; reject custom properties.
      if (
        Object.getPrototypeOf(node.children) !== Array.prototype ||
        Object.getOwnPropertySymbols(node.children).length ||
        Object.keys(node.children).join(",") !== "0,1"
      )
        fail("Invalid layout children");
      for (const index of [0, 1]) {
        const property = Object.getOwnPropertyDescriptor(
          node.children,
          String(index),
        );
        if (!property || !("value" in property))
          fail("Invalid layout children");
        visit(property.value, depth + 1);
      }
    } else fail("Invalid layout node type");
  };
  visit(input, 0);
  if (bytes(JSON.stringify(input)) > MAX_BYTES) fail("Layout exceeds 256 KiB");
  return input as Layout;
}

function copyLayout(
  node: Layout,
  nextId: (id: string) => string,
  nextConversation?: () => string | undefined,
): Layout {
  if (node.type === "area") {
    const conversation =
      node.view === "chat" ? nextConversation?.() : undefined;
    return {
      type: "area",
      id: nextId(node.id),
      view: node.view as ViewId,
      ...(conversation === undefined ? {} : { conversation }),
    };
  }
  return {
    type: "split",
    id: nextId(node.id),
    axis: node.axis,
    ratio: node.ratio,
    children: [
      copyLayout(node.children[0], nextId, nextConversation),
      copyLayout(node.children[1], nextId, nextConversation),
    ],
  };
}

export function portableLayout(layout: Layout): Layout {
  return copyLayout(validateLayout(layout), (id) => id);
}

export function instantiateLayout(
  layout: Layout,
  conversations: string[] = [],
): Layout {
  const portable = portableLayout(layout);
  conversations.forEach((id) => identifier(id, 100, "conversation ID"));
  const occupied = new Set<string>();
  const collect = (node: Layout) => {
    occupied.add(node.id);
    if (node.type === "split") node.children.forEach(collect);
  };
  collect(portable);
  let index = 0;
  return copyLayout(
    portable,
    () => {
      let id: string;
      do id = crypto.randomUUID();
      while (occupied.has(id));
      occupied.add(id);
      return id;
    },
    () => conversations[index++],
  );
}

export function encodeLayoutPreset(name: string, layout: Layout): string {
  const text = JSON.stringify(
    {
      format: "pcl-rh-code-layout",
      version: 1,
      name: nameValue(name),
      layout: portableLayout(layout),
    },
    null,
    2,
  );
  if (bytes(text) + 1 > MAX_BYTES) fail("Preset exceeds 256 KiB");
  return text + "\n";
}

export function decodeLayoutPreset(text: string): {
  name: string;
  layout: Layout;
} {
  if (typeof text !== "string" || bytes(text) > MAX_BYTES)
    fail("Preset exceeds 256 KiB");
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    fail("Invalid preset JSON");
  }
  const envelope = object(parsed);
  fields(envelope, ["format", "version", "name", "layout"]);
  if (envelope.format !== "pcl-rh-code-layout" || envelope.version !== 1)
    fail("Unsupported layout preset format or version");
  return {
    name: nameValue(envelope.name),
    layout: copyLayout(validateLayout(envelope.layout), (id) => id),
  };
}
