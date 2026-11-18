import type { SchemaObject } from "@/lib/api";

/**
 * Objects matching every word of `query` (case-insensitive, in `schema.name`), best first:
 * name equal to the last word, then name starting with it, then the rest; ties by name length.
 * A word with a dot ("shop.ord") matches across the schema and the name.
 */
export function searchObjects(objects: SchemaObject[], query: string, limit = 50): SchemaObject[] {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return [];
  const last = words.at(-1)!.split(".").at(-1)!;
  const rank = (o: SchemaObject) => {
    const n = o.name.toLowerCase();
    return n === last ? 0 : n.startsWith(last) ? 1 : 2;
  };
  return objects
    .filter((o) => {
      const full = `${o.schema}.${o.name}`.toLowerCase();
      return words.every((w) => full.includes(w));
    })
    .sort((a, b) => rank(a) - rank(b) || a.name.length - b.name.length || a.name.localeCompare(b.name))
    .slice(0, limit);
}
