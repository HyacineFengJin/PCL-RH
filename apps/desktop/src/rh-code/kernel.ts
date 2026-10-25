/** Bundled plugins are trusted code. The registry enforces contribution ownership
 * and Cordis effect lifetimes; it does not provide a JavaScript sandbox. A failed
 * owner retires its contributions and transitive dependents before retry.
 * Metadata/import thunks persist; factory instances belong to active scopes.
 * Reconciliation is serialized, and request versions cancel late imports/apply
 * results when a newer surface configuration or host disposal supersedes them. */
import { Context, type Fiber } from "@deepseek-ai/cordis";
import { launcherSlots } from "../productPlugins/surfaceTypes";
import type {
  LauncherSlot,
  LauncherSurfaceContribution,
  RegisteredLauncherSurface,
} from "../productPlugins/surfaceTypes";
import type {
  ExperimentalNavigate,
  ExtensionCatalog,
} from "../experimentalTypes";
import type {
  ActivePluginScope,
  CommandContribution,
  LayoutContribution,
  Manifest,
  PluginStatus,
  ViewContribution,
} from "./types";
export type ProductPluginServices = {
  /** Replace this activation's dynamic views in one registry publication. */
  replaceViews(views: ViewContribution[]): void;
};
export type ProductPlugin = {
  manifest: Manifest;
  launcherSurfaces?: LauncherSurfaceContribution[];
  views?: ViewContribution[];
  commands?: CommandContribution[];
  layouts?: LayoutContribution[];
  apply?: (
    ctx: Context,
    services: ProductPluginServices,
  ) => void | Promise<void>;
  /** A manifest without a usable bundled entry remains visible in settings. */
  loadError?: string;
};
export type PluginRuntime = {
  /** Narrow native lifecycle bridge; never the general Tauri API. */
  nativeServices?: { activate(owner: string): Promise<void> };
  /** Fixed product destinations only; a card never receives the native Api. */
  navigate?: ExperimentalNavigate;
  /** Hold an RH Code controller lease for background work. The plugin owns its
   * release and must register it with ctx.effect so retirement releases it. */
  keepAwake?: () => () => void;
  /** Metadata only. Community declarations never receive this trusted port. */
  community?: {
    readCatalog(): Promise<ExtensionCatalog>;
    subscribe(listener: () => void): () => void;
    isAvailable(): boolean;
  };
  commands?: {
    run: (...args: any[]) => unknown;
    stop: (...args: any[]) => unknown;
    openBuild?: (...args: any[]) => unknown;
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
const validComponent = (value: unknown) =>
  typeof value === "function" ||
  (typeof value === "object" &&
    value !== null &&
    "$$typeof" in value &&
    [Symbol.for("react.forward_ref"), Symbol.for("react.memo")].includes(
      value.$$typeof as symbol,
    ));
function validateView(view: ViewContribution) {
  if (
    typeof view.label !== "string" ||
    !view.label.trim() ||
    !validComponent(view.icon) ||
    !validComponent(view.render) ||
    (view.settings !== undefined && !validComponent(view.settings)) ||
    (view.tone !== undefined &&
      !["neutral", "blue", "purple", "orange"].includes(view.tone))
  )
    throw new Error(`Invalid view: ${view.id}`);
}
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
  if (m.dynamicViews !== undefined) {
    const declaration = m.dynamicViews;
    if (
      typeof declaration !== "object" ||
      declaration === null ||
      Array.isArray(declaration) ||
      Object.keys(declaration).length !== 2 ||
      !Object.hasOwn(declaration, "prefix") ||
      !Object.hasOwn(declaration, "limit") ||
      typeof declaration.prefix !== "string" ||
      declaration.prefix.length > 80 ||
      !declaration.prefix.startsWith(`${m.id}.`) ||
      !/^[a-zA-Z0-9][a-zA-Z0-9._-]*\.$/.test(declaration.prefix) ||
      !Number.isInteger(declaration.limit) ||
      declaration.limit < 1 ||
      declaration.limit > 384
    )
      throw new Error("Invalid dynamic view declaration");
  }
  ids(m.commands ?? [], "command IDs");
  if (
    m.runtimeNeeds !== undefined &&
    (!Array.isArray(m.runtimeNeeds) ||
      new Set(m.runtimeNeeds).size !== m.runtimeNeeds.length ||
      m.runtimeNeeds.some(
        (runtime) => runtime !== "native" && runtime !== "node",
      ))
  )
    throw new Error("Invalid runtime requirements");
  if (m.nativeServices !== undefined) {
    if (!Array.isArray(m.nativeServices))
      throw new Error("Invalid native service declarations");
    ids(
      m.nativeServices.map((service) => service?.id),
      "native service IDs",
    );
    if (
      m.nativeServices.some(
        (service) =>
          !Number.isSafeInteger(service.version) || service.version < 1,
      )
    )
      throw new Error("Invalid native service version");
  }
  ids(m.layouts ?? [], "layout IDs");
  if (
    !Array.isArray(m.capabilities) ||
    m.capabilities.some((v) => typeof v !== "string")
  )
    throw new Error("Invalid manifest capabilities");
  if (m.launcherSurfaces !== undefined) {
    if (
      !Array.isArray(m.launcherSurfaces) ||
      m.launcherSurfaces.length > 32 ||
      pluginScope(m) === "rh-code"
    )
      throw new Error("Invalid launcher surface declarations");
    ids(
      m.launcherSurfaces.map((surface) => surface?.id),
      "launcher surface IDs",
    );
    if (
      m.launcherSurfaces.some(
        (surface) =>
          !surface ||
          typeof surface !== "object" ||
          Object.keys(surface).length !== 2 ||
          !launcherSlots.includes(surface.slot),
      ) ||
      (m.launcherSurfaces.some(
        (surface) => surface.slot === "home.announcements",
      ) &&
        !m.capabilities.includes("launcher.announcements.read")) ||
      (m.launcherSurfaces.some(
        (surface) => surface.slot === "instance.overview.tools",
      ) &&
        !m.capabilities.includes("launcher.instances.export"))
    )
      throw new Error("Invalid launcher surface slot or capability");
  }
  const surfaces = plugin.launcherSurfaces ?? [];
  const seenSurfaces = new Set<string>();
  for (const surface of surfaces) {
    if (
      surface.owner !== m.id ||
      !validId(surface.id) ||
      !m.launcherSurfaces?.some(
        (declaration) =>
          declaration.id === surface.id && declaration.slot === surface.slot,
      ) ||
      !validComponent(surface.render) ||
      seenSurfaces.has(surface.id)
    )
      throw new Error(`Undeclared or invalid launcher surface: ${surface.id}`);
    seenSurfaces.add(surface.id);
  }
  if (m.tools !== undefined) {
    if (!Array.isArray(m.tools)) throw new Error("Invalid tool declarations");
    ids(
      m.tools.map((tool) => tool?.name),
      "tool names",
    );
    for (const tool of m.tools)
      if (
        tool.name.length > 80 ||
        !/^[a-z][a-z0-9_]*\.[a-z][a-z0-9_]*$/.test(tool.name) ||
        !m.capabilities.includes(tool.capability) ||
        !Array.isArray(tool.modes) ||
        !tool.modes.length ||
        new Set(tool.modes).size !== tool.modes.length ||
        tool.modes.some((mode) => !["ask", "plan", "execute"].includes(mode))
      )
        throw new Error(`Invalid tool scope: ${tool.name}`);
  }
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
  for (const view of plugin.views ?? []) validateView(view);
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
type ViewListing = Pick<ViewContribution, "id" | "owner" | "label">;
type ViewPublisher = {
  live: boolean;
  ready: boolean;
  views: Map<string, ViewContribution>;
};
type SurfaceOwner = {
  live: boolean;
  ready: boolean;
  records: RegisteredLauncherSurface[];
};
let nextSurfaceGeneration = 0;
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
  private knownViews = new Map<string, ViewListing>();
  private viewPublishers = new Map<string, ViewPublisher>();
  private launcherSurfaces = new Map<string, RegisteredLauncherSurface>();
  private surfaceOwners = new Map<string, SurfaceOwner>();
  private listeners = new Set<() => void>();
  private sequence: Promise<void> = Promise.resolve();
  private stopped = false;
  private disabled: readonly string[] = [];
  constructor(plugins: PluginDefinition[]) {
    if (new Set(plugins.map((p) => p.manifest.id)).size !== plugins.length)
      throw new Error("Duplicate product plugin");
    this.definitions = [...plugins];
    this.plugins = [...plugins];
    for (const p of plugins) {
      for (const id of p.manifest.views ?? [])
        if (!this.knownViews.has(id))
          this.knownViews.set(id, {
            id,
            owner: p.manifest.id,
            label:
              ("views" in p && p.views?.find((v) => v.id === id)?.label) || id,
          });
      this.statuses.set(p.manifest.id, {
        id: p.manifest.id,
        state: "dormant",
        dependencies: [...dependencies(p)],
      });
    }
  }
  /** Static metadata survives lazy scope retirement without retaining component
   * closures. Dynamic metadata exists only for the current live publication. */
  listViews(): ViewListing[] {
    const entries = new Map(this.knownViews);
    for (const view of this.views.values())
      entries.set(view.id, {
        id: view.id,
        owner: view.owner,
        label: view.label,
      });
    return [...entries.values()].map((view) => ({ ...view }));
  }
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  listLauncherSurfaces(slot: LauncherSlot): RegisteredLauncherSurface[] {
    const rank = (surface: RegisteredLauncherSurface) => {
      const owner = this.definitions.findIndex(
        (p) => p.manifest.id === surface.owner,
      );
      const item = this.definitions[owner].manifest.launcherSurfaces!.findIndex(
        (declaration) => declaration.id === surface.id,
      );
      return owner * 32 + item;
    };
    return [...this.launcherSurfaces.values()]
      .filter((surface) => surface.slot === slot && surface.lifetime.isActive())
      .sort((a, b) => rank(a) - rank(b));
  }
  private allowsOwner(
    id: string,
    disabled = this.disabled,
    scopes = this.scopes,
  ) {
    return (
      !this.stopped &&
      !this.definitions.some(({ manifest }) => {
        const scope = pluginScope(manifest);
        const excluded =
          (!manifest.required && disabled.includes(manifest.id)) ||
          (scope === "both" ? !scopes.length : !scopes.includes(scope));
        return excluded && this.dependents(manifest.id).has(id);
      })
    );
  }
  /** Withdraw UI before the host awaits native scope reconciliation. This
   * grants no new activation; configure remains the serialized state boundary. */
  revokeLauncherSurfaces(disabled: readonly string[], scopes = this.scopes) {
    for (const id of this.surfaceOwners.keys())
      if (!this.allowsOwner(id, disabled, scopes)) this.retireSurfaces(id);
    this.changed();
  }
  private retireSurfaces(id: string) {
    const owner = this.surfaceOwners.get(id);
    if (!owner) return;
    owner.live = false;
    this.surfaceOwners.delete(id);
    for (const surface of owner.records)
      if (this.launcherSurfaces.get(surface.id) === surface)
        this.launcherSurfaces.delete(surface.id);
  }
  private registerSurfaces(ctx: Context, plugin: ProductPlugin) {
    if (!plugin.launcherSurfaces?.length) return undefined;
    const id = plugin.manifest.id;
    const owner: SurfaceOwner = { live: false, ready: false, records: [] };
    const generation = ++nextSurfaceGeneration;
    const lifetime = Object.freeze({
      isActive: () =>
        owner.live &&
        owner.ready &&
        ctx.fiber.uid !== null &&
        this.owners.get(ctx) === plugin &&
        this.surfaceOwners.get(id) === owner &&
        this.allowsOwner(id),
    });
    owner.records = plugin.launcherSurfaces.map((surface) =>
      Object.freeze({ ...surface, generation, lifetime }),
    );
    ctx.effect(() => {
      if (
        owner.records.some((surface) => this.launcherSurfaces.has(surface.id))
      )
        throw new Error("Duplicate launcher surface");
      owner.live = true;
      this.surfaceOwners.set(id, owner);
      for (const surface of owner.records)
        this.launcherSurfaces.set(surface.id, surface);
      return () => {
        owner.live = false;
        if (this.surfaceOwners.get(id) === owner) {
          this.retireSurfaces(id);
          this.changed();
        }
      };
    });
    return owner;
  }
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
  private retireViews(id: string, publisher: ViewPublisher) {
    publisher.live = false;
    if (this.viewPublishers.get(id) === publisher)
      this.viewPublishers.delete(id);
    let removed = false;
    for (const [key, view] of publisher.views)
      if (this.views.get(key) === view) {
        this.views.delete(key);
        removed = true;
      }
    publisher.views.clear();
    return removed;
  }
  private viewServices(
    ctx: Context,
    plugin: ProductPlugin,
    current: () => boolean,
  ): { services: ProductPluginServices; publisher: ViewPublisher } {
    const id = plugin.manifest.id;
    const publisher: ViewPublisher = {
      live: false,
      ready: false,
      views: new Map(),
    };
    ctx.effect(() => {
      publisher.live = true;
      this.viewPublishers.set(id, publisher);
      return () => {
        if (this.retireViews(id, publisher)) this.changed();
      };
    });
    const available = () => {
      if (
        this.stopped ||
        ctx.fiber.uid === null ||
        !publisher.live ||
        this.viewPublishers.get(id) !== publisher ||
        this.owners.get(ctx) !== plugin ||
        (!publisher.ready && !current())
      )
        return false;
      // A newly requested scope/disable takes effect before queued async
      // reconciliation can dispose the old Cordis context.
      return !this.definitions.some((definition) => {
        const manifest = definition.manifest;
        const scope = pluginScope(manifest);
        const excluded =
          (!manifest.required && this.disabled.includes(manifest.id)) ||
          (scope === "both"
            ? this.scopes.length === 0
            : !this.scopes.includes(scope));
        return excluded && this.dependents(manifest.id).has(id);
      });
    };
    const services: ProductPluginServices = {
      replaceViews: (views) => {
        if (!available()) throw new Error(`Plugin views unavailable: ${id}`);
        const declaration = plugin.manifest.dynamicViews;
        if (!declaration) throw new Error(`Undeclared dynamic views: ${id}`);
        if (!Array.isArray(views) || views.length > declaration.limit)
          throw new Error(`Dynamic view quota exceeded: ${id}`);
        const next = new Map<string, ViewContribution>();
        // Copy before validation; mutations to the caller's descriptors cannot
        // change the ID/owner that the registry validated and published.
        for (const item of views) {
          const view = { ...item };
          if (
            view.owner !== id ||
            !validId(view.id) ||
            !view.id.startsWith(declaration.prefix)
          )
            throw new Error(`Undeclared or foreign dynamic view: ${view.id}`);
          validateView(view);
          if (
            next.has(view.id) ||
            this.definitions.some((p) => p.manifest.views.includes(view.id)) ||
            (this.views.has(view.id) &&
              this.views.get(view.id) !== publisher.views.get(view.id))
          )
            throw new Error(`Duplicate view ${view.id}`);
          next.set(view.id, Object.freeze(view));
        }
        // Getters on trusted descriptor objects may have triggered retirement
        // or another publication while the candidate was being copied.
        if (!available()) throw new Error(`Plugin views unavailable: ${id}`);
        for (const [key, view] of publisher.views)
          if (this.views.get(key) === view) this.views.delete(key);
        for (const [key, view] of next) this.views.set(key, view);
        publisher.views = next;
        this.changed();
      },
    };
    return { services, publisher };
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
      const publisher = this.viewPublishers.get(id);
      if (publisher) this.retireViews(id, publisher);
      this.retireSurfaces(id);
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
    for (const owner of this.dependents(id)) this.retireSurfaces(owner);
    this.changed();
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
    // Revoke component actions immediately. If a disable is superseded before
    // queued disposal runs, reconciliation still recreates this activation;
    // old buttons must never revive with the same enabled configuration.
    this.revokeLauncherSurfaces(this.disabled, this.scopes);
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
        if (
          off.has(p.manifest.id) ||
          !eligible(p) ||
          (this.fibers.has(p.manifest.id) &&
            p.manifest.launcherSurfaces?.length &&
            !this.surfaceOwners.has(p.manifest.id))
        )
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
            let publisher: ViewPublisher | undefined;
            let surfaceOwner: SurfaceOwner | undefined;
            fiber = this.context.plugin({
              name: id,
              apply: async (ctx: Context) => {
                this.owners.set(ctx, plugin);
                surfaceOwner = this.registerSurfaces(ctx, plugin);
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
                const publication = this.viewServices(ctx, plugin, current);
                publisher = publication.publisher;
                await plugin.apply?.(ctx, publication.services);
              },
            });
            this.fibers.set(id, fiber);
            await fiber;
            if (!current()) {
              await this.unload(new Set([id]));
              return;
            }
            if (publisher) publisher.ready = true;
            for (const surface of plugin.manifest.launcherSurfaces ?? [])
              if (this.launcherSurfaces.get(surface.id)?.owner !== id)
                throw new Error(
                  `Declared launcher surface missing: ${surface.id}`,
                );
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
            if (surfaceOwner) surfaceOwner.ready = true;
            for (const view of plugin.views ?? [])
              this.knownViews.set(view.id, {
                id: view.id,
                owner: view.owner,
                label: view.label,
              });
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
    for (const id of this.surfaceOwners.keys()) this.retireSurfaces(id);
    this.changed();
    await this.sequence.catch(() => {});
    await this.unload(new Set(this.fibers.keys()));
    for (const p of this.plugins) this.status(p.manifest.id, "stopped");
    this.changed();
  }
}
