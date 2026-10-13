import { Plus, Trash2 } from "lucide-react";
import { CeSelect } from "../../CeSelect";
import { label } from "../locale";
import type {
  FaceName,
  ModelDocument,
  ModelElement,
  ModelFace,
  ModelOperation,
  Vec3,
  ElementRotation,
  QuarterTurn,
} from "../../../../../code/visual-assets.mjs";

const faceNames: Record<FaceName, readonly [string, string]> = {
  north: ["北", "North"],
  south: ["南", "South"],
  east: ["东", "East"],
  west: ["西", "West"],
  up: ["上", "Up"],
  down: ["下", "Down"],
};
const faces = Object.keys(faceNames) as FaceName[];
function fullFaces(): ModelElement["faces"] {
  return Object.fromEntries(
    faces.map((face) => [face, { uv: [0, 0, 16, 16], rotation: 0 }]),
  ) as ModelElement["faces"];
}
function VectorInput({
  title,
  value,
  update,
}: {
  title: string;
  value: Vec3;
  update: (next: Vec3) => void;
}) {
  return (
    <fieldset className="rc-va-vector">
      <legend>{title}</legend>
      {value.map((coordinate, index) => (
        <label key={index}>
          {["X", "Y", "Z"][index]}
          <input
            aria-label={title + " " + ["X", "Y", "Z"][index]}
            type="number"
            min={-16}
            max={32}
            step={0.5}
            value={coordinate}
            onChange={(event) => {
              const next: Vec3 = [...value];
              next[index] = Number(event.target.value);
              update(next);
            }}
          />
        </label>
      ))}
    </fieldset>
  );
}
export function ModelGeometry({
  document,
  selected,
  select,
  edit,
}: {
  document: ModelDocument;
  selected: string;
  select: (id: string) => void;
  edit: (operations: ModelOperation[]) => void;
}) {
  const element =
    document.elements.find((value) => value.id === selected) ??
    document.elements[0];
  function patch(value: Extract<ModelOperation, { op: "update" }>["patch"]) {
    if (element) edit([{ op: "update", id: element.id, patch: value }]);
  }
  function updateRotation(value: Partial<ElementRotation>) {
    patch({
      rotation: {
        origin: [8, 8, 8],
        axis: "y",
        angle: 0,
        ...element?.rotation,
        ...value,
      },
    });
  }
  function updateFace(name: FaceName, face: ModelFace | null) {
    if (!element) return;
    const next = structuredClone(element.faces);
    if (face) next[name] = face;
    else delete next[name];
    patch({ faces: next });
  }
  return (
    <section className="rc-va-geometry">
      <div className="rc-va-section-heading">
        <strong>{label("方块元素", "Cuboids")}</strong>
        <small>{document.elements.length}/128</small>
        <button
          disabled={document.elements.length >= 128}
          aria-label={label("添加方块元素", "Add cuboid")}
          onClick={() => {
            let sequence = 1;
            while (
              document.elements.some((value) => value.id === "cube-" + sequence)
            )
              sequence++;
            const id = "cube-" + sequence;
            edit([
              {
                op: "add",
                element: {
                  id,
                  from: [0, 0, 0],
                  to: [16, 16, 16],
                  faces: fullFaces(),
                },
              },
            ]);
            select(id);
          }}
        >
          <Plus size={14} />
        </button>
      </div>
      <div
        className="rc-va-element-list"
        aria-label={label("模型元素列表", "Model element list")}
      >
        {document.elements.map((value) => (
          <button
            key={value.id}
            aria-pressed={element?.id === value.id}
            onClick={() => select(value.id)}
            title={value.id}
          >
            {value.id}
          </button>
        ))}
      </div>
      {element && (
        <>
          <div className="rc-va-section-heading">
            <strong title={element.id}>{element.id}</strong>
            <button
              aria-label={label("删除当前方块元素", "Remove selected cuboid")}
              title={label("删除元素", "Remove cuboid")}
              onClick={() => edit([{ op: "remove", id: element.id }])}
            >
              <Trash2 size={14} />
            </button>
          </div>
          <VectorInput
            title={label("起点", "From")}
            value={element.from}
            update={(from) => patch({ from })}
          />
          <VectorInput
            title={label("终点", "To")}
            value={element.to}
            update={(to) => patch({ to })}
          />
          <details className="rc-va-disclosure">
            <summary>{label("元素旋转", "Element rotation")}</summary>
            <label className="rc-va-checkbox">
              <input
                type="checkbox"
                checked={!!element.rotation}
                onChange={(event) =>
                  event.target.checked
                    ? updateRotation({})
                    : patch({ rotation: null })
                }
              />
              {label("启用旋转", "Enable rotation")}
            </label>
            {element.rotation && (
              <>
                <VectorInput
                  title={label("旋转中心", "Rotation origin")}
                  value={element.rotation.origin}
                  update={(origin) => updateRotation({ origin })}
                />
                <div className="rc-va-tool-row">
                  <label>
                    {label("轴", "Axis")}
                    <CeSelect
                      aria-label={label("旋转轴", "Rotation axis")}
                      value={element.rotation.axis}
                      onChange={(event) =>
                        updateRotation({
                          axis: event.target.value as ElementRotation["axis"],
                        })
                      }
                    >
                      {["x", "y", "z"].map((axis) => (
                        <option key={axis} value={axis}>
                          {axis.toUpperCase()}
                        </option>
                      ))}
                    </CeSelect>
                  </label>
                  <label>
                    {label("角度", "Angle")}
                    <CeSelect
                      aria-label={label(
                        "元素旋转角度",
                        "Element rotation angle",
                      )}
                      value={element.rotation.angle}
                      onChange={(event) =>
                        updateRotation({
                          angle: Number(
                            event.target.value,
                          ) as ElementRotation["angle"],
                        })
                      }
                    >
                      {[-45, -22.5, 0, 22.5, 45].map((angle) => (
                        <option key={angle} value={angle}>
                          {angle}°
                        </option>
                      ))}
                    </CeSelect>
                  </label>
                </div>
                <label className="rc-va-checkbox">
                  <input
                    type="checkbox"
                    checked={!!element.rotation.rescale}
                    onChange={(event) =>
                      updateRotation({ rescale: event.target.checked })
                    }
                  />
                  {label("旋转后缩放补偿", "Rotation rescale")}
                </label>
              </>
            )}
          </details>
          <details className="rc-va-disclosure" open>
            <summary>
              {label("每面 UV", "Face UVs")}
              <small>0–16</small>
            </summary>
            {faces.map((name) => {
              const face = element.faces[name],
                title = label(...faceNames[name]);
              return (
                <div className="rc-va-face" key={name}>
                  <label className="rc-va-checkbox">
                    <input
                      aria-label={title + " " + label("面", "face")}
                      type="checkbox"
                      checked={!!face}
                      onChange={(event) =>
                        updateFace(
                          name,
                          event.target.checked
                            ? { uv: [0, 0, 16, 16], rotation: 0 }
                            : null,
                        )
                      }
                    />
                    {title}
                  </label>
                  {face && (
                    <>
                      <div className="rc-va-uv">
                        {face.uv.map((value, index) => (
                          <label key={index}>
                            {["U₁", "V₁", "U₂", "V₂"][index]}
                            <input
                              aria-label={
                                title + " " + ["U1", "V1", "U2", "V2"][index]
                              }
                              type="number"
                              min={0}
                              max={16}
                              step={0.25}
                              value={value}
                              onChange={(event) => {
                                const uv: ModelFace["uv"] = [...face.uv];
                                uv[index] = Number(event.target.value);
                                updateFace(name, { ...face, uv });
                              }}
                            />
                          </label>
                        ))}
                      </div>
                      <label className="rc-va-face-rotation">
                        {label("材质旋转", "UV rotation")}
                        <CeSelect
                          aria-label={
                            title + " " + label("材质旋转", "UV rotation")
                          }
                          value={face.rotation ?? 0}
                          onChange={(event) =>
                            updateFace(name, {
                              ...face,
                              rotation: Number(
                                event.target.value,
                              ) as QuarterTurn,
                            })
                          }
                        >
                          {[0, 90, 180, 270].map((angle) => (
                            <option key={angle} value={angle}>
                              {angle}°
                            </option>
                          ))}
                        </CeSelect>
                      </label>
                    </>
                  )}
                </div>
              );
            })}
          </details>
        </>
      )}
    </section>
  );
}
