/** Pure split-tree edits preserve stable area identities and all project data.
 * A disabled plugin leaves its area in place so enabling it restores the view. */
import type { Area, Layout, ViewId } from "./types";
const id = () => crypto.randomUUID();
export const area = (view: ViewId, conversation?: string): Area => ({
  type: "area",
  id: id(),
  view,
  conversation,
});
export const split = (
  axis: "row" | "column",
  a: Layout,
  b: Layout,
  ratio = 0.5,
): Layout => ({ type: "split", id: id(), axis, ratio, children: [a, b] });
export function preset(name: string, conversations: string[] = []): Layout {
  if (name === "parallel")
    return split(
      "row",
      area("threads"),
      split(
        "row",
        area("chat", conversations[0]),
        area("chat", conversations[1]),
      ),
      0.2,
    );
  if (name === "source")
    return split(
      "row",
      area("files"),
      split(
        "row",
        split("column", area("source"), area("logs"), 0.7),
        area("chat", conversations[0]),
        0.6,
      ),
      0.2,
    );
  return split(
    "row",
    area("graph"),
    split("row", area("chat", conversations[0]), area("targets"), 0.72),
    0.2,
  );
}
export function leaves(node: Layout): Area[] {
  return node.type === "area" ? [node] : node.children.flatMap(leaves);
}
export function depth(node: Layout): number {
  return node.type === "area" ? 0 : 1 + Math.max(...node.children.map(depth));
}
export function map(node: Layout, fn: (area: Area) => Area): Layout {
  return node.type === "area"
    ? fn(node)
    : {
        ...node,
        children: node.children.map((n) => map(n, fn)) as [Layout, Layout],
      };
}
export function replace(node: Layout, target: string, next: Layout): Layout {
  if (node.id === target) return next;
  return node.type === "area"
    ? node
    : {
        ...node,
        children: node.children.map((n) => replace(n, target, next)) as [
          Layout,
          Layout,
        ],
      };
}
export function remove(node: Layout, target: string): Layout | null {
  if (node.id === target) return null;
  if (node.type === "area") return node;
  const children = node.children
    .map((n) => remove(n, target))
    .filter((n): n is Layout => !!n);
  return children.length === 1
    ? children[0]
    : { ...node, children: children as [Layout, Layout] };
}
export function exchange(node: Layout, a: string, b: string): Layout {
  const list = leaves(node),
    first = list.find((n) => n.id === a),
    second = list.find((n) => n.id === b);
  if (!first || !second) return node;
  return map(node, (n) => (n.id === a ? second : n.id === b ? first : n));
}
export function resize(node: Layout, target: string, ratio: number): Layout {
  if (node.type === "area") return node;
  return {
    ...node,
    ratio:
      node.id === target ? Math.max(0.15, Math.min(0.85, ratio)) : node.ratio,
    children: node.children.map((n) => resize(n, target, ratio)) as [
      Layout,
      Layout,
    ],
  };
}
