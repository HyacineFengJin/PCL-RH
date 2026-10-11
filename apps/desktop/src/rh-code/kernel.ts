/** Bundled plugins are trusted code. The registry enforces contribution ownership
 * and Cordis effect lifetimes; it does not provide a JavaScript sandbox. A failed
 * owner retires its contributions and transitive dependents before retry.
 * Metadata/import thunks persist; factory instances belong to active scopes.
 * Reconciliation is serialized, and request versions cancel late imports/apply
 * results when a newer surface configuration or host disposal supersedes them. */
import { Context, type Fiber } from "@deepseek-ai/cordis";
import type {
  ActivePluginScope,
  CommandContribution,
  LayoutContribution,
  Manifest,
  PluginStatus,
  ViewContribution,
} from "./types";
export type ProductPlugin = {
  manifest: Manifest;
  views?: ViewContribution[];
  commands?: CommandContribution[];
  layouts?: LayoutContribution[];
  apply?: (ctx: Context) => void | Promise<void>;
  /** A manifest without a usable bundled entry remains visible in settings. */
  loadError?: string;
};
export type PluginRuntime = {
  /** Hold an RH Code controller lease for background work. The plugin owns its
   * release and must register it with ctx.effect so retirement releases it. */
  keepAwake?: () => () => void;
  commands?: {
    run: (...args: any[]) => unknown;
    stop: (...args: any[]) => unknown;
  };
};
/** A *.plugin.tsx entry exports this value as its default export. The shared
 * JSON manifest supplies its identity, dependency and contribution authority. */
export type BundledPluginEntry = {
  id: string;
  create(manifest: Manifest, runtime: PluginRuntime): ProductPlugin;
};
/** Metadata and an import thunk survive retirement; the created instance does not. */
export type LazyProductPlugin = {
  manifest: Manifest;
  load: () => Promise<BundledPluginEntry>;
  runtime?: PluginRuntime;
};
export type PluginDefinition = ProductPlugin | LazyProductPlugin;
export const pluginScope = (manifest: Manifest) => manifest.scope ?? "rh-code";
const errorText = (error: unknown) =>
  error instanceof Error ? error.message : String(error);
const validId = (id: unknown): id is string =>
  typeof id === "string" &&
  id.length <= 120 &&
  /^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(id);
function ids(value: unknown, name: string): asserts value is string[] {
  if (
    !Array.isArray(value) ||
    value.some((id) => !validId(id)) ||
    new Set(value).size !== value.length
  )
    throw new Error(`Invalid or duplicate ${name}`);
}
const dependencies = (plugin: { manifest: Manifest }): string[] =>
  Array.isArray(plugin.manifest.dependencies)
    ? plugin.manifest.dependencies
    : [];
function validate(plugin: ProductPlugin) {
  const m = plugin.manifest;
  if (
    !validId(m.id) ||
    typeof m.name !== "string" ||
    !m.name.trim() ||
    typeof m.version !== "string" ||
    !m.version.trim() ||
    typeof m.required !== "boolean"
  )
    throw new Error("Invalid plugin manifest");
  if (!["launcher", "rh-code", "both"].includes(pluginScope(m)))
    throw new Error("Invalid plugin scope");
  ids(m.dependencies, "dependencies");
  ids(m.views, "view IDs");
  ids(m.commands ?? [], "command IDs");
  ids(m.layouts ?? [], "layout IDs");
  if (
    !Array.isArray(m.capabilities) ||
    m.capabilities.some((v) => typeof v !== "string")
  )
    throw new Error("Invalid manifest capabilities");
  for (const [kind, declared, contributions] of [
    ["view", m.views, plugin.views ?? []],
    ["command", m.commands ?? [], plugin.commands ?? []],
    ["layout", m.layouts ?? [], plugin.layouts ?? []],
  ] as const) {
    const seen = new Set<string>();
    for (const item of contributions) {
      if (
        item.owner !== m.id ||
        !validId(item.id) ||
        !declared.includes(item.id)
      )
        throw new Error(`Undeclared or foreign ${kind}: ${item.id}`);
      if (seen.has(item.id)) throw new Error(`Duplicate ${kind}: ${item.id}`);
      seen.add(item.id);
    }
  }
  for (const view of plugin.views ?? [])
    if (
      typeof view.label !== "string" ||
      !view.label.trim() ||
      !view.icon ||
      !view.render
    )
      throw new Error(`Invalid view: ${view.id}`);
  for (const command of plugin.commands ?? [])
    if (typeof command.handler !== "function")
      throw new Error(`Invalid command: ${command.id}`);
  for (const layout of plugin.layouts ?? [])
    if (
      typeof layout.label !== "string" ||
      !layout.label.trim() ||
      typeof layout.create !== "function"
    )
      throw new Error(`Invalid layout: ${layout.id}`);
  if (plugin.loadError) throw new Error(plugin.loadError);
}
export class Kernel {
  readonly context = new Context();
  readonly views = new Map<string, ViewContribution>();
  readonly commands = new Map<string, (...args: any[]) => unknown>();
  readonly layouts = new Map<string, LayoutContribution>();
  readonly statuses = new Map<string, PluginStatus>();
  readonly plugins: ProductPlugin[];
  private definitions: PluginDefinition[];
  private version = 0;
  private scopes: readonly ActivePluginScope[] = ["rh-code"];
  private fibers = new Map<string, Fiber>();
  private owners = new WeakMap<Context, ProductPlugin>();
  private commandOwners = new Map<string, string>();
  private listeners = new Set<() => void>();
  private sequence: Promise<void> = Promise.resolve();
  private stopped = false;
  private disabled: readonly string[] = [];
  constructor(plugins: PluginDefinition[]) {
    if (new Set(plugins.map((p) => p.manifest.id)).size !== plugins.length)
      throw new Error("Duplicate product plugin");
    this.definitions = [...plugins];
    this.plugins = [...plugins];
    for (const p of plugins)
      this.statuses.set(p.manifest.id, {
        id: p.manifest.id,
        state: "dormant",
        dependencies: [...dependencies(p)],
      });
  }
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  /** Legacy first-party apply hooks register only commands declared by their own
   * manifest. Direct registration on the root or another context is rejected. */
  registerCommand(
    ctx: Context,
    id: string,
    handler: (...args: any[]) => unknown,
  ) {
    const plugin = this.owners.get(ctx);
    if (!plugin || !plugin.manifest.commands?.includes(id))
      throw new Error(`Undeclared plugin command: ${id}`);
    if (typeof handler !== "function")
      throw new Error(`Invalid command: ${id}`);
    ctx.effect(() => {
      if (this.commands.has(id)) throw new Error(`Duplicate command ${id}`);
      // Consumers may retain a command reference from the public map. That
      // reference becomes unavailable when this registration is retired.
      const registered = (...args: any[]) => {
        if (this.commands.get(id) !== registered)
          throw new Error(`Plugin command unavailable: ${id}`);
        return handler(...args);
      };
      this.commands.set(id, registered);
      this.commandOwners.set(id, plugin.manifest.id);
      return () => {
        if (this.commands.get(id) === registered) {
          this.commands.delete(id);
          this.commandOwners.delete(id);
        }
      };
    });
  }
  invoke<T>(id: string, ...args: unknown[]): T {
    const handler = this.commands.get(id);
    if (!handler) throw new Error(`Plugin command unavailable: ${id}`);
    return handler(...args) as T;
  }
  private changed() {
    for (const listener of this.listeners) listener();
  }
  private status(
    id: string,
    state: PluginStatus["state"],
    detail: Partial<PluginStatus> = {},
  ) {
    this.statuses.set(id, { id, state, ...detail });
  }
  private enqueue(work: () => Promise<void>) {
    this.sequence = this.sequence.catch(() => {}).then(work);
    return this.sequence;
  }
  private dependents(id: string) {
    const affected = new Set([id]);
    let added = true;
    while (added) {
      added = false;
      for (const p of this.plugins)
        if (
          !affected.has(p.manifest.id) &&
          dependencies(p).some((d) => affected.has(d))
        ) {
          affected.add(p.manifest.id);
          added = true;
        }
    }
    return affected;
  }
  /** Registries are swept even when a plugin cleanup hook throws. Retiring
   * descendants before ancestors keeps their dependency effects available. */
  private async unload(ids: Set<string>) {
    const remaining = new Set(
      [...this.fibers.keys()].filter((id) => ids.has(id)),
    );
    while (remaining.size) {
      const id =
        [...remaining].find(
          (id) =>
            !this.plugins.some(
              (p) =>
                remaining.has(p.manifest.id) && dependencies(p).includes(id),
            ),
        ) ?? [...remaining][0];
      remaining.delete(id);
      const fiber = this.fibers.get(id)!;
      this.fibers.delete(id);
      // Remove published contributions before async disposal can yield to UI.
      for (const [key, view] of this.views)
        if (view.owner === id) this.views.delete(key);
      for (const [key, layout] of this.layouts)
        if (layout.owner === id) this.layouts.delete(key);
      for (const [key, owner] of this.commandOwners)
        if (owner === id) {
          this.commands.delete(key);
          this.commandOwners.delete(key);
        }
      this.changed();
      try {
        await fiber.dispose();
      } catch (error) {
        this.status(id, "failed", { error: errorText(error) });
      }
      this.owners.delete(fiber.ctx);
      // Release created contribution/component closures. ES module caching
      // belongs to the browser and cannot be cleared by the application.
      const index = this.definitions.findIndex((p) => p.manifest.id === id);
      this.plugins[index] = this.definitions[index];
    }
  }
  /** A host render boundary may report a plugin crash. Saved area data survives
   * retirement, allowing settings to retry the owner without rebuilding areas. */
  reportFailure(id: string, error: unknown) {
    if (!this.statuses.has(id) || this.stopped) return Promise.resolve();
    return this.enqueue(async () => {
      const affected = this.dependents(id);
      await this.unload(affected);
      this.status(id, "failed", { error: errorText(error) });
      for (const other of affected)
        if (other !== id)
          this.status(other, "blocked", {
            dependencies: dependencies(
              this.plugins.find((p) => p.manifest.id === other)!,
            ).filter((d) => !this.fibers.has(d)),
          });
      this.changed();
    });
  }
  /** Reconcile failed owners and their dependency chains using the current
   * enabled configuration. Existing active plugin instances remain intact. */
  retry(_id?: string) {
    return this.configure(this.disabled, this.scopes);
  }
  configure(
    disabled: readonly string[],
    activeScopes: readonly ActivePluginScope[] = this.scopes,
  ) {
    const requested = [...disabled];
    const scopes = [...activeScopes];
    if (scopes.some((scope) => scope !== "launcher" && scope !== "rh-code"))
      throw new Error("Invalid active plugin scope");
    const version = ++this.version;
    this.disabled = requested;
    this.scopes = scopes;
    const current = () => !this.stopped && version === this.version;
    const eligible = (p: { manifest: Manifest }) => {
      const scope = pluginScope(p.manifest);
      return scope === "both" ? scopes.length > 0 : scopes.includes(scope);
    };
    return this.enqueue(async () => {
      if (!current()) return;
      const off = new Set(
        this.definitions
          .filter(
            (p) => !p.manifest.required && requested.includes(p.manifest.id),
          )
          .map((p) => p.manifest.id),
      );
      const retire = new Set<string>();
      for (const p of this.definitions)
        if (off.has(p.manifest.id) || !eligible(p))
          for (const child of this.dependents(p.manifest.id)) retire.add(child);
      await this.unload(retire);
      if (!current()) return;
      const pending = new Map(
        this.definitions
          .filter((p) => !this.fibers.has(p.manifest.id))
          .map((p) => [p.manifest.id, p]),
      );
      for (const [id, definition] of pending) {
        if (off.has(id)) {
          this.status(id, "disabled");
          pending.delete(id);
          continue;
        }
        try {
          // Validate authority before importing executable modules. Launcher
          // lifetimes cannot require an owner that retires with RH Code.
          validate({ manifest: definition.manifest });
          if (pluginScope(definition.manifest) !== "rh-code")
            for (const dependency of dependencies(definition)) {
              const target = this.definitions.find(
                (p) => p.manifest.id === dependency,
              );
              if (target && pluginScope(target.manifest) === "rh-code")
                throw new Error(
                  `Plugin scope cannot depend on RH Code only plugin: ${dependency}`,
                );
            }
          if (!eligible(definition)) {
            this.status(id, "dormant");
            pending.delete(id);
          }
        } catch (error) {
          this.status(id, "failed", { error: errorText(error) });
          pending.delete(id);
        }
      }
      // Import after dependencies are active. Failed owners leave unrelated
      // modules available and their descendants blocked for a later retry.
      let progressed = true;
      while (progressed && current()) {
        progressed = false;
        for (const [id, definition] of pending) {
          if (!current()) return;
          if (dependencies(definition).some((d) => !this.fibers.has(d)))
            continue;
          pending.delete(id);
          progressed = true;
          let fiber: Fiber | undefined;
          try {
            let plugin: ProductPlugin;
            if ("load" in definition) {
              const entry = await definition.load();
              // A dispose or surface change during import prevents the factory
              // from acquiring resources after its requested lifetime.
              if (!current()) return;
              if (entry.id !== id)
                throw new Error("Bundled entry identity mismatch");
              plugin = entry.create(
                definition.manifest,
                definition.runtime ?? {},
              );
              if (plugin.manifest !== definition.manifest)
                throw new Error(
                  "Bundled entry replaced its authoritative manifest",
                );
            } else plugin = definition;
            validate(plugin);
            const index = this.definitions.findIndex(
              (p) => p.manifest.id === id,
            );
            this.plugins[index] = plugin;
            fiber = this.context.plugin({
              name: id,
              apply: async (ctx: Context) => {
                this.owners.set(ctx, plugin);
                for (const view of plugin.views ?? [])
                  ctx.effect(() => {
                    if (this.views.has(view.id))
                      throw new Error(`Duplicate view ${view.id}`);
                    this.views.set(view.id, view);
                    return () => {
                      if (this.views.get(view.id) === view)
                        this.views.delete(view.id);
                    };
                  });
                for (const layout of plugin.layouts ?? [])
                  ctx.effect(() => {
                    if (this.layouts.has(layout.id))
                      throw new Error(`Duplicate layout ${layout.id}`);
                    this.layouts.set(layout.id, layout);
                    return () => {
                      if (this.layouts.get(layout.id) === layout)
                        this.layouts.delete(layout.id);
                    };
                  });
                for (const command of plugin.commands ?? [])
                  this.registerCommand(ctx, command.id, command.handler);
                await plugin.apply?.(ctx);
              },
            });
            this.fibers.set(id, fiber);
            await fiber;
            if (!current()) {
              await this.unload(new Set([id]));
              return;
            }
            for (const viewId of plugin.manifest.views)
              if (this.views.get(viewId)?.owner !== id)
                throw new Error(`Declared view missing: ${viewId}`);
            for (const layoutId of plugin.manifest.layouts ?? [])
              if (this.layouts.get(layoutId)?.owner !== id)
                throw new Error(`Declared layout missing: ${layoutId}`);
            for (const commandId of plugin.manifest.commands ?? [])
              if (
                this.commandOwners.get(commandId) !== id ||
                !this.commands.has(commandId)
              )
                throw new Error(`Declared command missing: ${commandId}`);
            this.status(id, "active");
          } catch (error) {
            if (fiber) await this.unload(new Set([id]));
            const index = this.definitions.findIndex(
              (p) => p.manifest.id === id,
            );
            this.plugins[index] = this.definitions[index];
            if (!current()) return;
            this.status(id, "failed", { error: errorText(error) });
          }
          this.changed();
        }
      }
      for (const [id, p] of pending)
        this.status(id, "blocked", {
          dependencies: dependencies(p).filter((d) => !this.fibers.has(d)),
        });
      this.changed();
    });
  }
  async dispose() {
    this.stopped = true;
    ++this.version;
    await this.sequence.catch(() => {});
    await this.unload(new Set(this.fibers.keys()));
    for (const p of this.plugins) this.status(p.manifest.id, "stopped");
    this.changed();
  }
}
