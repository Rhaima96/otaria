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
 * (`FROM shop.users u`, `JOIN orders AS o`, `FROM a x, b y`). Lower-cased alias, unquoted names.
 */
export function namedTables(sql: string): { schema?: string; table: string; alias?: string }[] {
  const item = `(?:(${ID})\\.)?(${ID})(?:\\s+(?:as\\s+)?(${ID}))?`;
  const start = new RegExp(`\\b(from|join|update|into)\\s+`, "gi");
  // After FROM and UPDATE, a comma-separated list: one table, then ", table" as long as one follows.
  const next = new RegExp(`\\s*,\\s*${item}`, "iy");
  const out: { schema?: string; table: string; alias?: string }[] = [];
  for (const m of sql.matchAll(start)) {
    let at = m.index! + m[0].length;
    let found = new RegExp(item, "iy");
    for (;;) {
      found.lastIndex = at;
      const r = found.exec(sql);
      if (!r) break;
      const [, s, tb, a] = r;
      const keyword = !!a && NOT_ALIAS.has(a.toLowerCase());
      out.push({ schema: s ? unquote(s) : undefined, table: unquote(tb), alias: a && !keyword ? unquote(a).toLowerCase() : undefined });
      if (keyword || !/^(from|update)$/i.test(m[1])) break;
      at = found.lastIndex;
      found = next;
    }
  }
  return out;
}
