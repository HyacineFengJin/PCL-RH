/** Product plugin lifetime boundary. Startup reads shared plugin configuration
 * for launcher entries; editor controllers and Code entries remain deferred
 * until their declared scope is needed. */
import { useCallback, useEffect, useRef, useState } from "react";
import manifests from "../../../code/plugins.json";
import { WorkRetention } from "./rh-code/retention";
import { createWorkLeases, maySuspendCode } from "./rhCodeLifecycle";
import { createProductPluginRuntime } from "./productPluginRuntime";
import { label } from "./rh-code/locale";
import { createCommunityCatalogPort } from "./rh-code/communityCatalog";
import type { Kernel } from "./rh-code/kernel";
import type { Controller } from "./rh-code/controller";
import type { Manifest, ActivePluginScope } from "./rh-code/types";
import type { RHCode } from "./rh-code/RHCode";

type CodeProps = Parameters<typeof RHCode>[0];
type Props = Omit<CodeProps, "kernel" | "onController" | "onConfiguration"> & {
  visible: boolean;
  onPluginConfiguration?: (disabled: string[]) => void;
};
type Loaded = { View: typeof RHCode; kernel: Kernel };
export function RHCodeHost(props: Props) {
  const [loaded, setLoaded] = useState<Loaded | null>(null),
    [error, setError] = useState(""),
    [retry, setRetry] = useState(0);
  const current = useRef(props),
    controller = useRef<Controller | null>(null),
    kernel = useRef<Kernel | null>(null),
    disabled = useRef<string[]>([]),
    configurationRead = useRef(false),
    alive = useRef(true),
    mounted = useRef(false),
    queue = useRef(Promise.resolve()),
    awaySince = useRef<number | null>(null),
    visibilityEpoch = useRef(0),
    leases = useRef(createWorkLeases()).current,
    root = useRef<HTMLDivElement>(null);
  const nativeRuntime = useRef(
    createProductPluginRuntime(() => current.current),
  ).current;
  const community = useRef(createCommunityCatalogPort(props)).current;
  if (current.current.visible !== props.visible) {
    visibilityEpoch.current++;
    awaySince.current = props.visible ? null : Date.now();
  }
  current.current = props;
  community.setSurface(props);
  const enqueue = useCallback((operation: () => Promise<void>) => {
    queue.current = queue.current.then(operation).catch((e) => {
      if (alive.current) setError(String(e));
    });
  }, []);
  const ensureKernel = async () => {
    if (kernel.current) return kernel.current;
    const [{ Kernel }, { productPlugins }] = await Promise.all([
      import("./rh-code/kernel"),
      import("./rh-code/plugins"),
    ]);
    const owner = new Kernel(
      productPlugins(
        {
          run: (...args: Parameters<Controller["run"]>) => {
            if (!controller.current) throw new Error("RH Code is not active");
            return controller.current.run(...args);
          },
          stop: (id: string) => {
            if (!controller.current) throw new Error("RH Code is not active");
            return controller.current.stop(id);
          },
          openBuild: (areaId: string, projectId: string) => {
            if (!controller.current) throw new Error("RH Code is not active");
            return controller.current.openBuild(areaId, projectId);
          },
        },
        undefined,
        leases.acquire,
        (target) => {
          if (alive.current && mounted.current && current.current.visible)
            current.current.onNavigate(target);
        },
        community.runtime,
        current.current.native
          ? { activate: nativeRuntime.activate }
          : undefined,
      ),
    );
    kernel.current = owner;
    return owner;
  };
  const configureKernel = async (
    owner: Kernel,
    scopes: ActivePluginScope[],
  ) => {
    await nativeRuntime.configure(scopes);
    await owner.configure(disabled.current, scopes);
  };
  const readConfiguration = async () => {
    if (configurationRead.current) return;
    const data = await nativeRuntime.readConfiguration();
    disabled.current = data.disabledPlugins;
    current.current.onPluginConfiguration?.(data.disabledPlugins);
    configurationRead.current = true;
  };
  const bind = useCallback((next: Controller | null) => {
    controller.current = next;
  }, []);
  const configure = useCallback(
    (ids: string[]) => {
      disabled.current = ids;
      current.current.onPluginConfiguration?.(ids);
      configurationRead.current = true;
      enqueue(async () => {
        if (alive.current && kernel.current)
          await configureKernel(
            kernel.current,
            mounted.current ? ["launcher", "rh-code"] : ["launcher"],
          );
      });
    },
    [enqueue],
  );
  useEffect(() => {
    alive.current = true;
    community.setSurface(current.current);
    return () => {
      alive.current = false;
      community.retire();
      // Queue ownership also covers an entry import still in flight.
      enqueue(async () => {
        if (!alive.current) {
          try {
            await kernel.current?.dispose();
          } finally {
            await nativeRuntime.dispose();
          }
        }
      });
    };
  }, [enqueue, community]);
  useEffect(() => {
    community.publishSurface();
  }, [props.api, props.native, props.visible, community]);
  useEffect(() => {
    enqueue(async () => {
      if (!alive.current) return;
      if (current.current.visible) {
        await readConfiguration();
        const owner = await ensureKernel();
        const { RHCode: View } = await import("./rh-code/RHCode");
        if (!alive.current || !current.current.visible) {
          await configureKernel(owner, ["launcher"]);
          return;
        }
        await configureKernel(owner, ["launcher", "rh-code"]);
        if (!alive.current) return;
        mounted.current = true;
        // Keep this exact component instance on ordinary tab changes.
        setLoaded({ View, kernel: owner });
        setError("");
      } else if (
        !mounted.current &&
        (manifests as Manifest[]).some(
          (p) => p.scope === "launcher" || p.scope === "both",
        )
      ) {
        await readConfiguration();
        const owner = await ensureKernel();
        await configureKernel(owner, ["launcher"]);
      }
    });
  }, [props.visible, retry, enqueue]);
  useEffect(() => {
    if (props.visible || !loaded) return;
    const timer = setInterval(
      () =>
        enqueue(async () => {
          const c = controller.current,
            epoch = visibilityEpoch.current;
          const stillAway = () =>
            alive.current &&
            !current.current.visible &&
            epoch === visibilityEpoch.current &&
            !leases.size &&
            !root.current?.querySelector('[role="dialog"]');
          if (
            !c ||
            !mounted.current ||
            !stillAway() ||
            !maySuspendCode(
              Date.now(),
              awaySince.current,
              c.getLifecycleState(),
              leases.size,
            )
          )
            return;
          // A shared launcher plugin may still use the Node host. Retiring Code
          // must not stop that service. Otherwise release Node before committing
          // the controller's irreversible suspension, so a busy host can veto it.
          const shared = kernel.current?.plugins.some(
            (p) =>
              (p.manifest.scope === "launcher" ||
                p.manifest.scope === "both") &&
              (p.manifest.runtimeNeeds?.includes("node") ?? true) &&
              kernel.current?.statuses.get(p.manifest.id)?.state === "active",
          );
          if (current.current.native && !shared) {
            const result = await current.current.api<{
              released: boolean;
              busy: boolean;
            }>("experimental_suspend");
            if (result.busy || !stillAway()) return;
          }
          // Native inspection can await filesystem work. Activity that finished
          // during it restarts the idle clock; autosave itself must not do so.
          if (
            !maySuspendCode(
              Date.now(),
              awaySince.current,
              c.getLifecycleState(),
              leases.size,
            )
          )
            return;
          if (!(await c.prepareSuspend(stillAway))) return;
          mounted.current = false;
          controller.current = null;
          setLoaded(null);
          if (kernel.current)
            await configureKernel(kernel.current, ["launcher"]);
        }),
      15_000,
    );
    return () => clearInterval(timer);
  }, [props.visible, loaded, enqueue, leases]);
  return (
    <div
      ref={root}
      className="ce-main-page rh-code-host"
      hidden={!props.visible}
      data-code-state={loaded ? "active" : "dormant"}
    >
      {error && (
        <div className="rc-error" role="alert">
          <span>{error}</span>
          <button
            onClick={() => {
              setError("");
              setRetry((n) => n + 1);
            }}
          >
            {label("重试", "Retry")}
          </button>
        </div>
      )}
      <WorkRetention.Provider value={leases.acquire}>
        {loaded ? (
          <loaded.View
            {...props}
            kernel={loaded.kernel}
            onController={bind}
            onConfiguration={configure}
          />
        ) : props.visible && !error ? (
          <div className="loading">
            {label("正在加载 RH Code…", "Loading RH Code…")}
          </div>
        ) : null}
      </WorkRetention.Provider>
    </div>
  );
}
