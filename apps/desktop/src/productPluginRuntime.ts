/** Native scope ownership for one mounted product host. A lease is local to this
 * owner; late cleanup cannot retire another host's Code services. This module
 * imports no editor implementation, project controller or Node runtime. */
import type { Api } from "./types";
import type { ActivePluginScope } from "./rh-code/types";
type Surface = { api: Api; native: boolean };
type Configuration = { revision: string; disabledPlugins: string[] };
export function createProductPluginRuntime(surface: () => Surface) {
  let lease: { id: string; api: Api } | null = null;
  let disposed = false;
  let tail: Promise<void> = Promise.resolve();
  const enqueue = (operation: () => Promise<void>) => {
    const result = tail.then(operation);
    tail = result.catch(() => {});
    return result;
  };
  async function release() {
    const current = lease;
    if (!current) return;
    await current.api("rh_code_call", {
      operation: "plugin_runtime",
      args: { action: "close", lease: current.id },
    });
    if (lease === current) lease = null;
  }
  return {
    readConfiguration: () =>
      surface().api<Configuration>("rh_code_call", {
        operation: "plugin_configuration",
        args: {},
      }),
    configure: (scopes: readonly ActivePluginScope[]) =>
      enqueue(async () => {
        if (disposed) return;
        const current = surface();
        if (!current.native || !scopes.includes("rh-code")) {
          await release();
          return;
        }
        if (!lease) {
          const result = await current.api<{ lease: string }>("rh_code_call", {
            operation: "plugin_runtime",
            args: { action: "open" },
          });
          if (
            typeof result.lease !== "string" ||
            !result.lease ||
            result.lease.length > 64
          )
            throw new Error("Invalid native plugin scope lease");
          lease = { id: result.lease, api: current.api };
        }
        // Disposal can arrive while open is awaiting IPC. It must consume the
        // eventual reply and release that exact lease, including during imports.
        if (disposed) await release();
      }),
    activate: async (id: string) => {
      if (disposed) throw new Error("Product plugin runtime retired");
      const current = surface();
      if (!current.native) return;
      const status = await current.api<{ id: string; state: string }>(
        "rh_code_call",
        {
          operation: "native_plugin",
          args: { id },
        },
      );
      if (status.id !== id || status.state !== "active")
        throw new Error(`Native plugin unavailable: ${id}`);
    },
    dispose: () => {
      disposed = true;
      return enqueue(release);
    },
  };
}
