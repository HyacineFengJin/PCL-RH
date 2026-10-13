/** Private host-owned presets. RPC projections omit keys; jobs receive an
 * immutable selection/limit snapshot rather than reading mutable form state.
 * Invalid or externally changed stores are retained, never reset on save. */
import fs from "node:fs/promises";
import { constants } from "node:fs";
import path from "node:path";
import { randomUUID } from "node:crypto";
import {
  normalizePiSelection,
  publicPiSelection,
} from "./runtime/src/providers/pi-selection.mjs";
import {
  normalizeWorkLimits,
  PI_WORK_LIMITS,
  PI_LIMIT_RANGES,
} from "./runtime/src/providers/pi-limits.mjs";

const MAX_BYTES = 1_000_000;
const UUID = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/;
const fail = () =>
  new Error(
    "AI preset store is invalid or changed; the original file was retained",
  );
const ownKeys = (v, keys) =>
  v &&
  typeof v === "object" &&
  !Array.isArray(v) &&
  Object.keys(v).every((k) => keys.includes(k));
export class AiPresets {
  #file;
  #bytes = null;
  #records = [];
  #selectedId = null;
  #warning = null;
  constructor(root) {
    this.#file = path.join(root, "ai-presets.json");
  }
  async #read() {
    let file;
    try {
      file = await fs.open(
        this.#file,
        constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
      );
    } catch (error) {
      if (error.code === "ENOENT") return null;
      throw fail();
    }
    try {
      const stat = await file.stat();
      if (
        !stat.isFile() ||
        stat.nlink !== 1 ||
        stat.uid !== process.getuid() ||
        stat.size > MAX_BYTES ||
        stat.mode & 0o077
      )
        throw fail();
      const bytes = await file.readFile();
      if (bytes.length > MAX_BYTES) throw fail();
      return bytes;
    } finally {
      await file.close();
    }
  }
  async init() {
    try {
      this.#bytes = await this.#read();
      if (!this.#bytes) return this;
      const data = JSON.parse(this.#bytes);
      if (
        !ownKeys(data, ["schemaVersion", "selectedId", "presets"]) ||
        data.schemaVersion !== 1 ||
        !Array.isArray(data.presets) ||
        data.presets.length > 32
      )
        throw fail();
      const ids = new Set();
      const records = [];
      for (const record of data.presets) {
        if (
          !ownKeys(record, ["id", "name", "config", "limits"]) ||
          !UUID.test(record.id) ||
          ids.has(record.id)
        )
          throw fail();
        this.#name(record.name);
        const selection = await normalizePiSelection(record.config);
        const limits = normalizeWorkLimits(record.limits);
        ids.add(record.id);
        records.push({ ...record, selection, limits });
      }
      if (data.selectedId !== null && !ids.has(data.selectedId)) throw fail();
      this.#records = records;
      this.#selectedId = data.selectedId;
    } catch {
      this.#warning = fail().message;
    }
    return this;
  }
  #name(name) {
    if (
      typeof name !== "string" ||
      !name.trim() ||
      name.length > 80 ||
      /[\x00-\x1f\x7f]/.test(name)
    )
      throw new Error("Provide a preset name of 1–80 characters");
  }
  view() {
    return {
      selectedId: this.#selectedId,
      presets: this.#records.map(({ id, name, selection, limits }) => ({
        id,
        name,
        selection: publicPiSelection(selection),
        limits,
      })),
      defaults: PI_WORK_LIMITS,
      ranges: PI_LIMIT_RANGES,
      warning: this.#warning,
    };
  }
  selected() {
    return (
      this.#records.find((record) => record.id === this.#selectedId) ?? null
    );
  }
  async #commit(records, selectedId) {
    if (this.#warning) throw fail();
    const current = await this.#read();
    if (
      (current === null) !== (this.#bytes === null) ||
      (current && !current.equals(this.#bytes))
    )
      throw fail();
    const bytes = Buffer.from(
      JSON.stringify({
        schemaVersion: 1,
        selectedId,
        presets: records.map(({ selection, ...record }) => record),
      }),
    );
    if (bytes.length > MAX_BYTES)
      throw new Error("AI presets exceed the storage limit");
    const temporary = this.#file + "." + randomUUID() + ".new";
    const file = await fs.open(temporary, "wx", 0o600);
    try {
      await file.writeFile(bytes);
      await file.sync();
      await file.close();
      const recheck = await this.#read();
      if (
        (recheck === null) !== (current === null) ||
        (recheck && !recheck.equals(current))
      )
        throw fail();
      await fs.rename(temporary, this.#file);
      const folder = await fs.open(
        path.dirname(this.#file),
        constants.O_RDONLY | constants.O_DIRECTORY,
      );
      try {
        await folder.sync();
      } finally {
        await folder.close();
      }
      // Publish in-memory only after durable replacement succeeds.
      this.#bytes = bytes;
      this.#records = records;
      this.#selectedId = selectedId;
    } finally {
      await file.close().catch(() => {});
      await fs.unlink(temporary).catch(() => {});
    }
  }
  async save({ id = null, name, config, limits }) {
    this.#name(name);
    const previous = this.#records.find((record) => record.id === id);
    if (id !== null && !previous) throw new Error("Unknown AI preset");
    if (!previous && this.#records.length >= 32)
      throw new Error("At most 32 AI presets can be saved");
    // Omitted key means retain, but only for the same provider and route.
    // An explicit empty key clears it for unauthenticated custom services.
    const next = { ...config };
    if (!Object.hasOwn(next, "key")) {
      if (
        !previous ||
        previous.selection.provider !== next.provider ||
        (previous.selection.custom &&
          (previous.selection.api !== next.api ||
            typeof next.baseUrl !== "string" ||
            previous.selection.baseUrl !==
              next.baseUrl.trim().replace(/\/+$/, "")))
      )
        throw new Error("Provide a key for the new provider or endpoint");
      next.key = previous.config.key;
    }
    const selection = await normalizePiSelection(next);
    const work = normalizeWorkLimits(limits);
    const record = {
      id: previous?.id ?? randomUUID(),
      name: name.trim(),
      config: {
        provider: selection.provider,
        model: selection.id,
        thinking: selection.thinking,
        key: selection.key,
        ...(selection.custom
          ? {
              api: selection.api,
              baseUrl: selection.baseUrl,
              contextWindow: selection.contextWindow,
              supportsImages: selection.supportsImages === true,
              cost: selection.cost,
            }
          : {}),
      },
      selection,
      limits: work,
    };
    await this.#commit(
      [...this.#records.filter((v) => v.id !== record.id), record],
      record.id,
    );
    return this.view();
  }
  async select(id) {
    if (!this.#records.some((record) => record.id === id))
      throw new Error("Unknown AI preset");
    await this.#commit(this.#records, id);
    return this.view();
  }
  async remove(id) {
    if (!this.#records.some((record) => record.id === id))
      throw new Error("Unknown AI preset");
    const records = this.#records.filter((record) => record.id !== id);
    await this.#commit(
      records,
      this.#selectedId === id ? (records[0]?.id ?? null) : this.#selectedId,
    );
    return this.view();
  }
}
