import { useEffect, useState } from "react";
import { CeSelect } from "./CeSelect";

// Keep this list explicit: the viewer can only load bundled notices, never a
// user-supplied path or a remote document. Raw imports become separate assets.
const documents = {
  LICENSE: () => import("../../../LICENSE?raw"),
  NOTICE: () => import("../../../NOTICE?raw"),
  "COPYRIGHT.md": () => import("../../../COPYRIGHT.md?raw"),
  "THIRD_PARTY_NOTICES.md": () => import("../../../THIRD_PARTY_NOTICES.md?raw"),
  "THIRDPARTY.generated.json": () =>
    import("../../../THIRDPARTY.generated.json?raw"),
  "LICENSES/DEPENDENCIES.txt": () =>
    import("../../../LICENSES/DEPENDENCIES.txt?raw"),
  "LICENSES/CORDIS.txt": () => import("../../../LICENSES/CORDIS.txt?raw"),
  "fonts/LICENSE": () => import("./assets/fonts/LICENSE?raw"),
  "fonts/NOTICE": () => import("./assets/fonts/NOTICE?raw"),
  "game-icons/NOTICE": () => import("./assets/game-icons/NOTICE?raw"),
  "game-icons/SOURCES.json": () =>
    import("./assets/game-icons/SOURCES.json?raw"),
  "game-icons/FABRIC-CC0.md": () =>
    import("./assets/game-icons/FABRIC-CC0.md?raw"),
  "game-icons/NEOFORGE-CC-BY.txt": () =>
    import("./assets/game-icons/NEOFORGE-CC-BY.txt?raw"),
  "game-icons/MINECRAFT-LICENSE.md": () =>
    import("./assets/game-icons/MINECRAFT-LICENSE.md?raw"),
  "game-icons/HMCL-GPL.txt": () =>
    import("./assets/game-icons/HMCL-GPL.txt?raw"),
  "credits/NOTICE": () => import("./assets/credits/NOTICE?raw"),
  "experimental/NOTICE.md": () => import("../../../experimental/NOTICE.md?raw"),
  "maker/LICENSE": () =>
    import("../../../experimental/runtime/vendor/maker/LICENSE?raw"),
  "porter/LICENSE": () =>
    import("../../../experimental/runtime/vendor/porter/LICENSE?raw"),
};
type DocumentId = keyof typeof documents;

export function LegalDocuments({
  title,
  loading,
  unavailable,
  retry,
}: {
  title: string;
  loading: string;
  unavailable: string;
  retry: string;
}) {
  const [open, setOpen] = useState(false);
  const [selected, setSelected] = useState<DocumentId>("LICENSE");
  const [attempt, setAttempt] = useState(0);
  const [result, setResult] = useState<{
    id: DocumentId;
    text?: string;
    failed?: boolean;
  }>();

  useEffect(() => {
    if (!open) return;
    let current = true;
    // Retire a slow import when selection changes or the viewer closes, so a
    // previous document cannot appear under the new document's heading.
    documents[selected]().then(
      ({ default: text }) => current && setResult({ id: selected, text }),
      () => current && setResult({ id: selected, failed: true }),
    );
    return () => {
      current = false;
    };
  }, [open, selected, attempt]);

  const visible = result?.id === selected ? result : undefined;
  return (
    <details
      className="extra-license-reader"
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary>{title}</summary>
      {open && (
        <>
          <CeSelect
            className="ce-field"
            aria-label={title}
            value={selected}
            onChange={(event) => {
              const id = event.target.value;
              if (Object.hasOwn(documents, id)) setSelected(id as DocumentId);
            }}
          >
            {Object.keys(documents).map((id) => (
              <option key={id} value={id}>
                {id}
              </option>
            ))}
          </CeSelect>
          {visible?.failed ? (
            <div className="ce-actions extra-actions" role="status">
              <span>{unavailable}</span>
              <button
                className="ce-button"
                onClick={() => {
                  setResult(undefined);
                  setAttempt((value) => value + 1);
                }}
              >
                {retry}
              </button>
            </div>
          ) : (
            <pre aria-label={selected} tabIndex={0}>
              {visible?.text ?? loading}
            </pre>
          )}
        </>
      )}
    </details>
  );
}
