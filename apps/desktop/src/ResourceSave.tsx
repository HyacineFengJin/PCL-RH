import { useEffect, useRef, useState } from "react";
import type { Api } from "./types";
import type { ResourceInstallRequest } from "./resourceInstallPlan";
import { t, serviceError } from "./i18n";
import {
  InstanceOperationDialog,
  instanceOperationSize,
  useInstanceOperationScope,
} from "./instanceOperationUi";
import "./instance-operations.css";

type SavePreview = {
  token: string;
  preferencesRevision: string;
  target: string;
  plan: {
    request: ResourceInstallRequest;
    project_title: string;
    version_name: string;
    file_name: string;
    size: number;
    sha512: string;
    sha1?: string | null;
    revision: string;
  };
};
function matchesSelection(
  preview: SavePreview,
  request: ResourceInstallRequest,
) {
  const plan = preview.plan;
  return (
    !!preview.token &&
    !!preview.preferencesRevision &&
    preview.target.startsWith("/") &&
    !!plan.revision &&
    (plan.request.source ?? "Modrinth") === (request.source ?? "Modrinth") &&
    plan.request.project_id === request.project_id &&
    plan.request.version_id === request.version_id &&
    plan.request.file_name === request.file_name &&
    plan.file_name === request.file_name &&
    Number.isSafeInteger(plan.size) &&
    plan.size >= 0 &&
    (request.source === "CurseForge"
      ? plan.sha512 === "" && /^[0-9a-f]{40}$/i.test(plan.sha1 ?? "")
      : /^[0-9a-f]{128}$/i.test(plan.sha512))
  );
}

/** Standalone Save has no instance target. The native chooser supplies the path
 * and a short-lived token binds official file authority, preferences and target.
 * Only that token is submitted after explicit review; closing a read never
 * starts a task, while admitted work transfers cancellation to TaskManager. */
export function ResourceSave({
  api,
  request,
  contextKey,
  native,
  disabled,
  startDisabled = false,
  startDisabledReason,
  onTaskStart,
  onClose,
}: {
  api: Api;
  request: ResourceInstallRequest;
  contextKey: string;
  native: boolean;
  disabled: boolean;
  startDisabled?: boolean;
  startDisabledReason?: string;
  onTaskStart: (id: string) => void;
  onClose: () => void;
}) {
  const identity = JSON.stringify([
    contextKey,
    request.source ?? "Modrinth",
    request.project_id,
    request.version_id,
    request.file_name,
  ]);
  const owner = useInstanceOperationScope(api, identity, native, disabled);
  const scope = owner.scope;
  const submissionBlocked = useRef(startDisabled);
  submissionBlocked.current = startDisabled;
  const closed = useRef<object | null>(null);
  const current = () => owner.current() && closed.current !== scope;
  const [state, setState] = useState<{
    scope: object;
    preview: SavePreview | null;
    error: string;
    invalid: boolean;
  } | null>(null);
  const operation = useRef<{ scope: object; kind: "prepare" | "start" } | null>(
    null,
  );
  const [activity, setActivity] = useState<typeof operation.current>(null);
  const callbacks = useRef({ onTaskStart, onClose });
  callbacks.current = { onTaskStart, onClose };
  const working = activity?.scope === scope ? activity.kind : null;
  const visible = state?.scope === scope ? state : null;
  const mountedScope = useRef<object | null>(null);
  function close() {
    if (
      !current() ||
      (operation.current?.scope === scope && operation.current.kind === "start")
    )
      return;
    closed.current = scope;
    operation.current = null;
    setActivity(null);
    setState(null);
    callbacks.current.onClose();
  }
  async function prepare() {
    if (!current() || !owner.allowed() || operation.current?.scope === scope)
      return;
    const captured = { scope, kind: "prepare" as const };
    operation.current = captured;
    setActivity(captured);
    setState({ scope, preview: null, error: "", invalid: false });
    try {
      const preview = await api<SavePreview | null>("resource_save_prepare", {
        request: {
          ...(request.source ? { source: request.source } : {}),
          project_id: request.project_id,
          version_id: request.version_id,
          file_name: request.file_name,
        },
      });
      if (!current() || operation.current !== captured) return;
      if (!preview) {
        close();
        return;
      }
      if (!matchesSelection(preview, request)) {
        setState({ scope, preview: null, error: "", invalid: true });
        return;
      }
      setState({ scope, preview, error: "", invalid: false });
    } catch (error) {
      if (current() && operation.current === captured)
        setState({
          scope,
          preview: null,
          error: String(error),
          invalid: false,
        });
    } finally {
      if (current() && operation.current === captured) {
        operation.current = null;
        setActivity(null);
      }
    }
  }
  useEffect(() => {
    if (mountedScope.current === scope) return;
    mountedScope.current = scope;
    void prepare();
  }, [scope]);
  async function start() {
    if (
      !current() ||
      !owner.allowed() ||
      submissionBlocked.current ||
      !visible?.preview ||
      operation.current?.scope === scope
    )
      return;
    const preview = visible.preview;
    if (!matchesSelection(preview, request)) return;
    const captured = { scope, kind: "start" as const };
    operation.current = captured;
    setActivity(captured);
    try {
      const task = await api<{ id: string }>("resource_save_start", {
        token: preview.token,
      });
      if (!current() || operation.current !== captured) return;
      if (!task.id) throw Error(t("save.missingTask"));
      setState(null);
      callbacks.current.onTaskStart(task.id);
      callbacks.current.onClose();
    } catch (error) {
      if (current() && operation.current === captured)
        setState({
          scope,
          preview: null,
          error: String(error),
          invalid: false,
        });
    } finally {
      if (current() && operation.current === captured) {
        operation.current = null;
        setActivity(null);
      }
    }
  }
  const preview = visible?.preview;
  return (
    <InstanceOperationDialog
      title={t("save.title")}
      titleId="ce-resource-save-title"
      busy={!!working}
      committing={working === "start"}
      confirmLabel={
        working === "start"
          ? t("save.starting")
          : preview
            ? t("save.confirm")
            : t("save.chooseAgain")
      }
      confirmDisabled={
        !owner.allowed() || !!working || (!!preview && startDisabled)
      }
      onConfirm={() => void (preview ? start() : prepare())}
      onClose={close}
    >
      <p>{t("save.help")}</p>
      {preview && startDisabled && (
        <p role="status">{startDisabledReason || t("common.working")}</p>
      )}
      {working === "prepare" && <p>{t("save.preparing")}</p>}
      {preview && (
        <>
          <div
            className="ce-operation-plan"
            style={{ overflowWrap: "anywhere" }}
          >
            <p>
              {t("save.project")}: {preview.plan.project_title}
            </p>
            <p>
              {t("save.version")}: {preview.plan.version_name}
            </p>
            <p>
              {t("save.file")}: {preview.plan.file_name}
            </p>
            <p>
              {t("save.size")}: {instanceOperationSize(preview.plan.size)}
            </p>
            <p>
              {request.source === "CurseForge"
                ? `SHA1: ${preview.plan.sha1}`
                : `SHA512: ${preview.plan.sha512}`}
            </p>
            <p>
              {t("save.target")}: {preview.target}
            </p>
          </div>
          <p>{t("save.noOverwrite")}</p>
        </>
      )}
      {visible?.invalid && (
        <p className="rd-name-error" role="alert">
          {t("save.selectionChanged")}
        </p>
      )}
      {visible?.error && (
        <p className="rd-name-error" role="alert">
          {serviceError(visible.error)}
        </p>
      )}
    </InstanceOperationDialog>
  );
}
