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

const ID = "`(?:[^`]|``)+`|\\w+";
const unquote = (s: string) => (s.startsWith("`") ? s.slice(1, -1).replace(/``/g, "`") : s);
/** Words that may follow a table name and are not an alias. */
const NOT_ALIAS = new Set("where on using join inner left right cross natural straight_join set values select group order having limit window union except intersect for lock as partition use force ignore returning".split(" "));

/**
 * Tables named after FROM / JOIN / UPDATE / INTO in a script, with their alias if any
 * (`FROM shop.users u`, `JOIN orders AS o`). Lower-cased alias, unquoted names.
 */
export function namedTables(sql: string): { schema?: string; table: string; alias?: string }[] {
  const re = new RegExp(`\\b(?:from|join|update|into)\\s+(?:(${ID})\\.)?(${ID})(?:\\s+(?:as\\s+)?(${ID}))?`, "gi");
  return [...sql.matchAll(re)].map(([, s, tb, a]) => {
    const alias = a && !NOT_ALIAS.has(a.toLowerCase()) ? unquote(a).toLowerCase() : undefined;
    return { schema: s ? unquote(s) : undefined, table: unquote(tb), alias };
  });
}
