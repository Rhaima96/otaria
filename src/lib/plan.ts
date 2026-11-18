import type { Plan } from "@/components/PlanNode.vue";

/** Generic walk: scalars stay on the node, objects and arrays become children. Works for any plan shape. */
function build(label: string, v: unknown): Plan {
  const node: Plan = { label, scalars: {}, children: [] };
  const entries: [string, unknown][] = Array.isArray(v) ? v.map((x, i) => [`${label} ${i + 1}`, x]) : Object.entries(v as object);
  for (const [k, x] of entries) {
    // possible_keys: ["a", "b"] and other lists of scalars read best on one line.
    if (Array.isArray(x) && x.every((y) => y === null || typeof y !== "object")) node.scalars[k] = x.join(", ");
    else if (x !== null && typeof x === "object") node.children.push(build(k, x));
    else node.scalars[k] = x;
  }
  // {"table": {...}} and other one-key wrappers: show the inner node directly.
  if (!Object.keys(node.scalars).length && node.children.length === 1 && !Array.isArray(v)) return node.children[0];
  return node;
}

/** EXPLAIN / ANALYZE FORMAT=JSON text → tree, or null if it is not a plan. */
export function parsePlan(text: string | null | undefined): { root: Plan; total: number } | null {
  if (!text || !text.trimStart().startsWith("{")) return null;
  try {
    const json = JSON.parse(text);
    if (!json?.query_block) return null;
    const total = Number(json.query_block.r_total_time_ms ?? json.query_optimization?.r_total_time_ms ?? 0);
    return { root: build("query", json), total };
  } catch {
    return null;
  }
}
