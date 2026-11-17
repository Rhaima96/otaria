// Fails if a locale misses a key from en.json, or a .vue template contains hard-coded text.
// usage: node scripts/check-i18n.mjs
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";

const flat = (o, p = "") =>
  Object.entries(o).flatMap(([k, v]) => (typeof v === "object" ? flat(v, `${p}${k}.`) : [`${p}${k}`]));

const dir = "src/locales";
const base = new Set(flat(JSON.parse(readFileSync(join(dir, "en.json"), "utf8"))));
let errors = 0;

for (const file of readdirSync(dir).filter((f) => f.endsWith(".json") && f !== "en.json")) {
  const keys = new Set(flat(JSON.parse(readFileSync(join(dir, file), "utf8"))));
  for (const k of base) if (!keys.has(k)) (errors++, console.error(`${file}: missing "${k}"`));
  for (const k of keys) if (!base.has(k)) (errors++, console.error(`${file}: extra "${k}" (add it to en.json first)`));
}

// Words allowed as literal text: brand names and symbols, never translated.
const ALLOWED = /^(Otaria|for MariaDB|MariaDB|[✓⚠✕·—\s\d.:]+)$/;
const walk = (d) => readdirSync(d).flatMap((f) => (statSync(join(d, f)).isDirectory() ? walk(join(d, f)) : [join(d, f)]));
for (const file of walk("src").filter((f) => f.endsWith(".vue") && !f.includes("components/ui/"))) {
  const tpl = readFileSync(file, "utf8").match(/<template>([\s\S]*)<\/template>/)?.[1] ?? "";
  const text = tpl.replace(/<[^>]*>/g, "\n").replace(/\{\{[\s\S]*?\}\}/g, "\n");
  for (const raw of text.split("\n")) {
    const s = raw.trim();
    if (s && /\p{L}/u.test(s) && !ALLOWED.test(s)) (errors++, console.error(`${file}: hard-coded text "${s}"`));
  }
}

if (errors) process.exit(1);
console.log(`i18n OK: ${base.size} keys × ${readdirSync(dir).length} locales`);
