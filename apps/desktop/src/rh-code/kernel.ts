/** Trusted product plugins use DSH's Cordis lifecycle. Registering a view is
 * an owned effect: unloading removes contributions before React drops them.
 * Cordis is composition, not a sandbox; community data extensions continue
 * through the native permission broker rather than arbitrary JS imports. */
import { Context, type Fiber } from "@deepseek-ai/cordis";
import type { Manifest, ViewContribution } from "./types";
export type ProductPlugin = {
  manifest: Manifest;
  views?: ViewContribution[];
  apply?: (ctx: Context) => void;
};
export class Kernel {
  readonly context = new Context();
  readonly views = new Map<string, ViewContribution>();
  readonly commands = new Map<string, (...args: any[]) => unknown>();
  private fibers = new Map<string, Fiber>();
  private listeners = new Set<() => void>();
  private sequence: Promise<void> = Promise.resolve();
  private stopped = false;
  readonly plugins: ProductPlugin[];
  constructor(plugins: ProductPlugin[]) {
    const ordered: ProductPlugin[] = [],
      remaining = new Map(plugins.map((p) => [p.manifest.id, p]));
    if (remaining.size !== plugins.length)
      throw new Error("Duplicate product plugin");
    while (remaining.size) {
      const ready = [...remaining.values()].filter((p) =>
        p.manifest.dependencies.every((id) =>
          ordered.some((q) => q.manifest.id === id),
        ),
      );
      if (!ready.length)
        throw new Error("Missing or cyclic product plugin dependency");
      for (const p of ready) {
        remaining.delete(p.manifest.id);
        ordered.push(p);
      }
    }
    this.plugins = ordered;
  }
  subscribe = (f: () => void) => {
    this.listeners.add(f);
    return () => {
      this.listeners.delete(f);
    };
  };
  registerCommand(
    ctx: Context,
    id: string,
    handler: (...args: any[]) => unknown,
  ) {
    ctx.effect(() => {
      if (this.commands.has(id)) throw new Error(`Duplicate command ${id}`);
      this.commands.set(id, handler);
      return () => {
        this.commands.delete(id);
      };
    });
  }
  invoke<T>(id: string, ...args: unknown[]): T {
    const handler = this.commands.get(id);
    if (!handler) throw new Error(`Plugin command unavailable: ${id}`);
    return handler(...args) as T;
  }
  private changed() {
    for (const f of this.listeners) f();
  }
  configure(disabled: readonly string[]) {
    this.sequence = this.sequence
      .catch(() => {})
      .then(async () => {
        if (this.stopped) return;
        for (const plugin of [...this.plugins].reverse())
          if (
            disabled.includes(plugin.manifest.id) &&
            this.fibers.has(plugin.manifest.id)
          ) {
            await this.fibers.get(plugin.manifest.id)!.dispose();
            this.fibers.delete(plugin.manifest.id);
          }
        for (const plugin of this.plugins) {
          if (
            disabled.includes(plugin.manifest.id) ||
            this.fibers.has(plugin.manifest.id)
          )
            continue;
          if (plugin.manifest.dependencies.some((d) => !this.fibers.has(d)))
            continue;
          const fiber = this.context.plugin({
            name: plugin.manifest.id,
            apply: (ctx: Context) => {
              for (const view of plugin.views ?? [])
                ctx.effect(() => {
                  if (this.views.has(view.id))
                    throw new Error(`Duplicate view ${view.id}`);
                  this.views.set(view.id, view);
                  return () => {
                    this.views.delete(view.id);
                  };
                });
              plugin.apply?.(ctx);
            },
          });
          try {
            await fiber;
            this.fibers.set(plugin.manifest.id, fiber);
          } catch (e) {
            await fiber.dispose();
            throw e;
          }
        }
        this.changed();
      });
    return this.sequence;
  }
  async dispose() {
    this.stopped = true;
    await this.sequence.catch(() => {});
    for (const fiber of [...this.fibers.values()].reverse())
      await fiber.dispose();
    this.fibers.clear();
    this.changed();
  }
}
