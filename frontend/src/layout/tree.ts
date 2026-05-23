import type { LayoutNode, SplitDir } from "../state/types";

export function leafCount(t: LayoutNode): number {
  switch (t.type) {
    case "empty": return 0;
    case "leaf": return 1;
    case "split": return leafCount(t.a) + leafCount(t.b);
  }
}

export function leaves(t: LayoutNode): string[] {
  const out: string[] = [];
  const visit = (n: LayoutNode) => {
    if (n.type === "leaf") out.push(n.agentId);
    else if (n.type === "split") {
      visit(n.a);
      visit(n.b);
    }
  };
  visit(t);
  return out;
}

/**
 * Split a specific leaf in two, with `newAgentId` becoming the second pane.
 */
export function splitLeaf(
  tree: LayoutNode,
  targetLeafId: string,
  direction: SplitDir,
  newAgentId: string,
): LayoutNode {
  if (tree.type === "empty") {
    return { type: "leaf", agentId: newAgentId };
  }
  if (tree.type === "leaf") {
    if (tree.agentId === targetLeafId) {
      return {
        type: "split",
        direction,
        ratio: 0.5,
        a: tree,
        b: { type: "leaf", agentId: newAgentId },
      };
    }
    return tree;
  }
  return {
    ...tree,
    a: splitLeaf(tree.a, targetLeafId, direction, newAgentId),
    b: splitLeaf(tree.b, targetLeafId, direction, newAgentId),
  };
}

/**
 * Auto-grid insert: append a new leaf into the smaller subtree, alternating
 * direction with depth. Used when no specific target is given (e.g. orchestrator
 * spawning N agents at once).
 */
export function insertGrid(
  tree: LayoutNode,
  newAgentId: string,
  depth = 0,
): LayoutNode {
  if (tree.type === "empty") return { type: "leaf", agentId: newAgentId };
  if (tree.type === "leaf") {
    return {
      type: "split",
      direction: depth % 2 === 0 ? "v" : "h",
      ratio: 0.5,
      a: tree,
      b: { type: "leaf", agentId: newAgentId },
    };
  }
  const aN = leafCount(tree.a);
  const bN = leafCount(tree.b);
  if (aN <= bN) {
    return { ...tree, a: insertGrid(tree.a, newAgentId, depth + 1) };
  }
  return { ...tree, b: insertGrid(tree.b, newAgentId, depth + 1) };
}

/**
 * Remove a leaf, collapsing now-empty splits.
 */
export function closeLeaf(tree: LayoutNode, leafId: string): LayoutNode {
  switch (tree.type) {
    case "empty": return tree;
    case "leaf": return tree.agentId === leafId ? { type: "empty" } : tree;
    case "split": {
      const a = closeLeaf(tree.a, leafId);
      const b = closeLeaf(tree.b, leafId);
      if (a.type === "empty") return b;
      if (b.type === "empty") return a;
      return { ...tree, a, b };
    }
  }
}

/**
 * Update the ratio of the split that contains the path.
 * `path` is a list of "a"|"b" steps to reach the split node.
 */
export function setRatioAt(
  tree: LayoutNode,
  path: ("a" | "b")[],
  ratio: number,
): LayoutNode {
  if (path.length === 0) {
    if (tree.type !== "split") return tree;
    return { ...tree, ratio };
  }
  if (tree.type !== "split") return tree;
  const [head, ...rest] = path;
  if (head === "a") return { ...tree, a: setRatioAt(tree.a, rest, ratio) };
  return { ...tree, b: setRatioAt(tree.b, rest, ratio) };
}

/**
 * Replace a leaf's agentId with a new one (used by respawn to swap dead→new).
 */
export function replaceLeafId(
  tree: LayoutNode,
  oldId: string,
  newId: string,
): LayoutNode {
  switch (tree.type) {
    case "empty": return tree;
    case "leaf": return tree.agentId === oldId ? { type: "leaf", agentId: newId } : tree;
    case "split": return {
      ...tree,
      a: replaceLeafId(tree.a, oldId, newId),
      b: replaceLeafId(tree.b, oldId, newId),
    };
  }
}

/** Serialize-safe deep clone (we deal in plain JSON). */
export function clone<T>(v: T): T {
  return JSON.parse(JSON.stringify(v));
}
