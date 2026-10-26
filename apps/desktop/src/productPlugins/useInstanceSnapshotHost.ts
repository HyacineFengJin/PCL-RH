import { useEffect, useMemo, useRef } from "react";
import type { Api } from "../types";
import type { LauncherSurfaceHost } from "./surfaceTypes";
import { createInstanceSnapshotConnection } from "./instanceSnapshots";
import { createInstanceCopyConnection } from "./instanceCopy";
import { createBackupRestoreConnection } from "./backupRestore";

/** Shared selection/admission binding for official instance tools. The shell
 * owns no copy/backup form, preview or workflow; those belong to the plugins. */
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
  const copyConnection = useMemo(
    () =>
      createInstanceCopyConnection({
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
  const restore = useMemo(
    () =>
      createBackupRestoreConnection({
        api,
        isCurrent: () =>
          current.current.live &&
          current.current.owner === owner &&
          current.current.available,
        onTask: (task, reveal) => current.current.onTask(task, reveal),
      }),
    [api, owner],
  );
  return {
    ...host,
    instanceCopy: {
      connection: copyConnection,
      suggestedName: `${id ?? ""} - Copy`,
      available: available && !!id,
      localization: host.announcements.localization,
    },
    instanceSnapshots: {
      connection,
      restore,
      available: available && !!id,
      localization: host.announcements.localization,
    },
  };
}
