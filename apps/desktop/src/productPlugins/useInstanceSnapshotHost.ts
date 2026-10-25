import { useEffect, useMemo, useRef } from "react";
import type { Api } from "../types";
import type { LauncherSurfaceHost } from "./surfaceTypes";
import { createInstanceSnapshotConnection } from "./instanceSnapshots";

/** The shell supplies its current selection and admission state. It owns no
 * backup form, preview, preset or task workflow; those belong to the plugin. */
export function useInstanceSnapshotHost(
  host: LauncherSurfaceHost,
  api: Api,
  id: string | undefined,
  context: string,
  available: boolean,
  onTask: (id: string, reveal?: boolean) => void,
): LauncherSurfaceHost {
  const owner = useMemo(() => ({}), [api, id, context, available]);
  const current = useRef({ owner, available, onTask, live: true });
  current.current = { owner, available, onTask, live: current.current.live };
  useEffect(() => {
    current.current.live = true;
    return () => {
      current.current.live = false;
    };
  }, []);
  const connection = useMemo(
    () =>
      createInstanceSnapshotConnection({
        api,
        id: id ?? "",
        isCurrent: () =>
          current.current.live &&
          current.current.owner === owner &&
          current.current.available,
        onTask: (task, reveal) => current.current.onTask(task, reveal),
      }),
    [api, id, owner],
  );
  return {
    ...host,
    instanceSnapshots: {
      connection,
      available: available && !!id,
      localization: host.announcements.localization,
    },
  };
}
