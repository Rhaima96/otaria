<script setup lang="ts">
import { computed, onMounted, ref, toRaw } from "vue";
import { useI18n } from "vue-i18n";
import { ArrowDown, ArrowUp, Plus, Save, Trash2, Undo2 } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type Algorithm, type AppError, type ColumnDef, type IndexDef, type TableDef } from "@/lib/api";
import { cn, versionAtLeast } from "@/lib/utils";

/** `table` null = new table. */
const props = defineProps<{ sessionId: string; schema: string; table: string | null; mariadb: boolean; version: string }>();
/** Indexes the optimizer ignores: MariaDB IGNORED, MySQL 8 INVISIBLE (none on 5.7). */
const canIgnore = props.mariadb || versionAtLeast(props.version, [8, 0, 0]);
const emit = defineEmits<{ saved: [name: string] }>();
const { t } = useI18n();

const EMPTY: TableDef = { name: "", engine: "", collation: "", comment: "", columns: [], indexes: [], fks: [], checks: [], versioned: false, periods: [], partitions: "" };
/** As loaded (null while creating): the diff base. */
const old = ref<TableDef | null>(null);
const def = ref<TableDef>(structuredClone(EMPTY));
const engines = ref<string[]>([]);
// Suggestions, loaded when the designer opens (schemas, collations) or when a field gets focus (tables, columns).
const collations = ref<string[]>([]);
const schemas = ref<string[]>([]);
const refTables = ref<Record<string, string[]>>({});
const refCols = ref<Record<string, string[]>>({});
const error = ref<AppError | null>(null);
const section = ref<"columns" | "indexes" | "fks" | "checks" | "options">("columns");
const selected = ref(0);

const clone = <T,>(v: T): T => structuredClone(toRaw(v));
const dirty = computed(() => JSON.stringify(def.value) !== JSON.stringify(old.value ?? EMPTY));
// Read by SessionView before closing the tab.
defineExpose({ dirty });

async function load(name = props.table) {
  error.value = null;
  try {
    [engines.value, collations.value, schemas.value] = await Promise.all([
      api.listEngines(props.sessionId),
      api.listCollations(props.sessionId),
      api.listSchemas(props.sessionId),
    ]);
    if (name) {
      old.value = await api.tableDef(props.sessionId, props.schema, name);
      def.value = clone(old.value);
    } else {
      def.value = { ...clone(EMPTY), engine: engines.value[0] ?? "", columns: [newColumn("id", "int")] };
      def.value.columns[0].autoIncrement = true;
      def.value.indexes.push({ original: null, name: "PRIMARY", kind: "PRIMARY", columns: [{ name: "id", length: null, desc: false }], comment: "", ignored: false });
    }
  } catch (e) {
    error.value = toAppError(e);
  }
}
onMounted(() => load());

async function loadTables(schema: string) {
  if (refTables.value[schema]) return;
  refTables.value[schema] = []; // once per schema, even if it fails
  const objs = await api.listObjects(props.sessionId, schema).catch(() => []);
  refTables.value[schema] = objs.filter((o) => o.kind === "table").map((o) => o.name);
}

async function loadCols(schema: string, table: string) {
  const key = `${schema}.${table}`;
  if (!table || refCols.value[key]) return;
  refCols.value[key] = [];
  refCols.value[key] = (await api.tableInfo(props.sessionId, schema, table).catch(() => null))?.columns.map((c) => c.name) ?? [];
}

function newColumn(name = "", colType = "varchar(255)"): ColumnDef {
  return { original: null, name, colType, nullable: !name, default: { kind: "none" }, generated: null, autoIncrement: false, onUpdate: null, invisible: false, collation: null, check: null, comment: "", unversioned: false };
}

function move<T>(list: T[], i: number, by: number) {
  const j = i + by;
  if (j < 0 || j >= list.length) return;
  [list[i], list[j]] = [list[j], list[i]];
  if (list === (def.value.columns as unknown)) selected.value = j;
}

const col = computed(() => def.value.columns[selected.value]);
/** Application periods (SYSTEM_TIME is the versioning's, edited in the Versioning tab). */
const appPeriods = computed(() => def.value.periods.filter((p) => p.name !== "SYSTEM_TIME"));
const dateCols = computed(() => def.value.columns.filter((c) => /^(date|datetime|timestamp)/i.test(c.colType)).map((c) => c.name));
const periodName = ref("");
function addPeriod() {
  const name = periodName.value.trim();
  if (!name || name.toLowerCase() === "system_time" || def.value.periods.some((p) => p.name.toLowerCase() === name.toLowerCase())) return;
  def.value.periods.push({ name, start: dateCols.value[0], end: dateCols.value[1] });
  periodName.value = "";
}
function removePeriod(name: string) {
  def.value.periods = def.value.periods.filter((p) => p.name !== name);
  def.value.indexes.forEach((i) => i.overlaps === name && (i.overlaps = null));
}
function setDefaultKind(c: ColumnDef, kind: string) {
  c.default = kind === "none" || kind === "null" ? { kind } : { kind: kind as "literal" | "expr", value: "value" in c.default ? c.default.value : "" };
}
function toggleGenerated(c: ColumnDef, on: boolean) {
  c.generated = on ? { expr: "", stored: false } : null;
}

// Index / FK columns are edited as "a, b(10) DESC, (expr)" text; a part in parentheses is an expression (MySQL 8).
const indexCols = (i: IndexDef) =>
  i.columns.map((c) => (c.expr ? `(${c.expr})` : c.length ? `${c.name}(${c.length})` : c.name) + (c.desc ? " DESC" : "")).join(", ");
/** Splits on the commas outside parentheses and quotes, so an expression keeps its own commas. */
function topLevelParts(text: string) {
  const parts: string[] = [];
  let depth = 0;
  let quote = "";
  let start = 0;
  for (let k = 0; k < text.length; k++) {
    const ch = text[k];
    if (quote) {
      if (ch === quote) quote = "";
    } else if (ch === "'" || ch === '"' || ch === "`") quote = ch;
    else if (ch === "(") depth++;
    else if (ch === ")") depth--;
    else if (ch === "," && depth === 0) {
      parts.push(text.slice(start, k));
      start = k + 1;
    }
  }
  parts.push(text.slice(start));
  return parts.map((p) => p.trim()).filter(Boolean);
}
function setIndexCols(i: IndexDef, text: string) {
  i.columns = topLevelParts(text).map((s) => {
    const [, body, dir] = s.match(/^(.*?)(\s+(?:DESC|ASC))?$/i)!;
    const desc = /desc/i.test(dir ?? "");
    if (body.startsWith("(") && body.endsWith(")")) return { name: "", length: null, desc, expr: body.slice(1, -1).trim() };
    const m = body.match(/^(.*?)(?:\s*\((\d+)\))?$/)!;
    return { name: m[1].trim(), length: m[2] ? Number(m[2]) : null, desc };
  });
}
const list = (v: string[]) => v.join(", ");
const parseList = (s: string) => s.split(",").map((x) => x.trim()).filter(Boolean);

// Preview, then run. ALTER tries INSTANT, then online (INPLACE, LOCK=NONE), then the server's choice:
// each step only after the server refused the previous one and the user agreed.
const dialog = ref<HTMLDialogElement | null>(null);
const preview = ref<string[]>([]);
const algorithm = ref<Algorithm>("instant");
const applyError = ref<AppError | null>(null);
const busy = ref(false);
const NEXT: Record<Algorithm, Algorithm | null> = { instant: "online", online: "default", default: null };

async function openPreview(algo: Algorithm = "instant") {
  error.value = null;
  applyError.value = null;
  algorithm.value = algo;
  try {
    preview.value = await api.designerSql(props.sessionId, props.schema, old.value, def.value, algo, wait.value);
    if (!preview.value.length) return;
    if (!dialog.value?.open) dialog.value?.showModal();
  } catch (e) {
    error.value = toAppError(e);
    dialog.value?.close();
  }
}

// MariaDB: lock wait of the ALTER (null = server's lock_wait_timeout, 0 = NOWAIT). Both servers: progress
// while the ALTER runs (MySQL: in-place InnoDB ALTERs, no stage numbers), with a Stop button (KILL QUERY).
const WAITS = [null, 0, 5, 30, 300] as const;
const wait = ref<number | null>(null);
const progress = ref<[number, number, number] | null>(null);
let jobId = "";

async function apply() {
  busy.value = true;
  applyError.value = null;
  jobId = crypto.randomUUID();
  progress.value = null;
  const timer = window.setInterval(async () => {
    progress.value = (await api.alterProgress(props.sessionId, jobId).catch(() => null)) ?? progress.value;
  }, 700);
  try {
    await api.designerApply(props.sessionId, props.schema, old.value, def.value, algorithm.value, wait.value, jobId);
    dialog.value?.close();
    const name = def.value.name.trim();
    await load(name);
    emit("saved", name);
  } catch (e) {
    applyError.value = toAppError(e);
  } finally {
    clearInterval(timer);
    busy.value = false;
    progress.value = null;
  }
}

const RULES = ["RESTRICT", "CASCADE", "SET NULL", "NO ACTION", "SET DEFAULT"];
// VECTOR indexes: MariaDB 11.7+ (an older server refuses them with its own error).
const KINDS: IndexDef["kind"][] = ["PRIMARY", "UNIQUE", "INDEX", "FULLTEXT", "SPATIAL", ...(props.mariadb ? (["VECTOR"] as const) : [])];
const fkSchema = (refSchema: string) => refSchema.trim() || props.schema;
// Every type, with a typical size where one is needed; any other spelling can still be typed.
const TYPES = [
  ..."tinyint tinyint(1) smallint mediumint int bigint".split(" "),
  "int unsigned",
  "bigint unsigned",
  ..."decimal(10,2) float double bit(1)".split(" "),
  ..."char(36) varchar(255) binary(16) varbinary(255) tinytext text mediumtext longtext tinyblob blob mediumblob longblob json".split(" "),
  "enum('a','b')",
  "set('a','b')",
  ..."date time datetime datetime(6) timestamp timestamp(6) year".split(" "),
  ..."geometry point linestring polygon multipoint multilinestring multipolygon geometrycollection".split(" "),
  // MariaDB only: uuid 10.7, inet4 10.10, inet6 10.5, vector 11.7 (an older server refuses them itself).
  ...(props.mariadb ? ["uuid", "inet4", "inet6", "vector(384)"] : []),
];

const input = "h-6 w-full min-w-0 rounded-sm border bg-background px-1.5 font-mono text-xs";
const cell = "border-b px-1 py-0.5 align-middle";
const head = "border-b px-1 py-1 text-left font-medium";
const btn = "flex h-6 items-center gap-1 rounded-sm px-2 hover:bg-secondary disabled:opacity-40 disabled:hover:bg-transparent";
const icon = "flex size-6 items-center justify-center rounded-sm text-muted-foreground hover:bg-secondary hover:text-foreground";
const subtab = "h-full border-b-2 px-3 font-medium";
</script>

<template>
  <div class="flex flex-col">
    <div class="flex h-9 shrink-0 items-stretch gap-1 border-b px-2">
      <button
        v-for="s in ['columns', 'indexes', 'fks', 'checks', 'options'] as const"
        :key="s"
        :class="cn(subtab, section === s ? 'border-primary text-foreground' : 'border-transparent text-muted-foreground hover:text-foreground')"
        @click="section = s"
      >
        {{ t(`designer.${s}`) }}
      </button>
      <div class="ml-auto flex items-center gap-1">
        <button :class="btn" :disabled="!dirty" @click="old ? (def = clone(old)) : load()"><Undo2 class="size-3.5" />{{ t("grid.discard") }}</button>
        <button
          :disabled="!dirty"
          class="flex h-6 items-center gap-1 rounded-sm bg-primary px-2 font-semibold text-primary-foreground disabled:opacity-40"
          @click="openPreview()"
        >
          <Save class="size-3.5" />{{ old ? t("designer.apply") : t("designer.create") }}
        </button>
      </div>
    </div>

    <ErrorLine v-if="error" :error="error" class="m-2" />

    <div class="min-h-0 flex-1 overflow-auto p-2">
      <!-- Columns: core fields in the table, the rest for the selected column below. -->
      <template v-if="section === 'columns'">
        <table class="w-full border-collapse">
          <thead class="text-muted-foreground">
            <tr>
              <th :class="head">{{ t("structure.name") }}</th>
              <th :class="head">{{ t("structure.type") }}</th>
              <th :class="head">{{ t("structure.nullable") }}</th>
              <th :class="head">{{ t("structure.default") }}</th>
              <th :class="head">{{ t("designer.autoIncrement") }}</th>
              <th :class="head">{{ t("structure.comment") }}</th>
              <th :class="head" />
            </tr>
          </thead>
          <tbody>
            <tr v-for="(c, i) in def.columns" :key="i" :class="cn('cursor-default', selected === i && 'bg-selection')" @click="selected = i">
              <td :class="cell"><input v-model="c.name" :aria-label="t('structure.name')" :class="input" /></td>
              <td :class="cell"><input v-model="c.colType" list="designer-types" :aria-label="t('structure.type')" :class="input" /></td>
              <td :class="cn(cell, 'text-center')"><input v-model="c.nullable" type="checkbox" :disabled="!!c.generated" :aria-label="t('structure.nullable')" /></td>
              <td :class="cell">
                <span v-if="c.generated" class="text-muted-foreground">{{ t("designer.generated") }}</span>
                <div v-else class="flex gap-1">
                  <select :value="c.default.kind" :aria-label="t('structure.default')" :class="cn(input, 'w-28 font-sans')" @change="setDefaultKind(c, ($event.target as HTMLSelectElement).value)">
                    <option value="none">{{ t("designer.defaultNone") }}</option>
                    <option value="null">NULL</option>
                    <option value="literal">{{ t("designer.defaultText") }}</option>
                    <option value="expr">{{ t("designer.defaultExpr") }}</option>
                  </select>
                  <input v-if="c.default.kind === 'literal' || c.default.kind === 'expr'" v-model="c.default.value" :aria-label="t('structure.default')" :class="input" />
                </div>
              </td>
              <td :class="cn(cell, 'text-center')"><input v-model="c.autoIncrement" type="checkbox" :disabled="!!c.generated" :aria-label="t('designer.autoIncrement')" /></td>
              <td :class="cell"><input v-model="c.comment" :aria-label="t('structure.comment')" :class="cn(input, 'font-sans')" /></td>
              <td :class="cn(cell, 'whitespace-nowrap')">
                <button :class="icon" :title="t('designer.up')" @click.stop="move(def.columns, i, -1)"><ArrowUp class="size-3.5" /><span class="sr-only">{{ t("designer.up") }}</span></button>
                <button :class="icon" :title="t('designer.down')" @click.stop="move(def.columns, i, 1)"><ArrowDown class="size-3.5" /><span class="sr-only">{{ t("designer.down") }}</span></button>
                <button :class="icon" :title="t('designer.remove')" @click.stop="def.columns.splice(i, 1), (selected = Math.max(0, i - 1))">
                  <Trash2 class="size-3.5" /><span class="sr-only">{{ t("designer.remove") }}</span>
                </button>
              </td>
            </tr>
          </tbody>
        </table>
        <button :class="cn(btn, 'mt-1')" @click="def.columns.splice(selected + 1, 0, newColumn()), (selected = Math.min(selected + 1, def.columns.length - 1))">
          <Plus class="size-3.5" />{{ t("designer.addColumn") }}
        </button>

        <fieldset v-if="col" class="mt-4 grid max-w-3xl grid-cols-[11rem_1fr] items-center gap-x-3 gap-y-1.5 rounded-md border p-3">
          <legend class="px-1 font-semibold">{{ t("designer.columnDetails", { name: col.name }) }}</legend>
          <label class="text-muted-foreground" for="d-gen">{{ t("designer.generated") }}</label>
          <div class="flex items-center gap-2">
            <input id="d-gen" type="checkbox" :checked="!!col.generated" @change="toggleGenerated(col, ($event.target as HTMLInputElement).checked)" />
            <template v-if="col.generated">
              <input v-model="col.generated.expr" :placeholder="t('designer.expression')" :aria-label="t('designer.expression')" :class="input" />
              <label class="flex shrink-0 items-center gap-1"><input v-model="col.generated.stored" type="checkbox" />{{ t("designer.stored") }}</label>
            </template>
          </div>
          <label class="text-muted-foreground" for="d-upd">{{ t("designer.onUpdate") }}</label>
          <input id="d-upd" :value="col.onUpdate ?? ''" :disabled="!!col.generated" placeholder="current_timestamp()" :class="input" @input="col.onUpdate = ($event.target as HTMLInputElement).value || null" />
          <label class="text-muted-foreground" for="d-coll">{{ t("designer.collation") }}</label>
          <input id="d-coll" :value="col.collation ?? ''" list="designer-collations" :placeholder="t('designer.tableDefault')" :class="input" @input="col.collation = ($event.target as HTMLInputElement).value || null" />
          <label class="text-muted-foreground" for="d-chk">{{ t("designer.check") }}</label>
          <input id="d-chk" :value="col.check ?? ''" :class="input" @input="col.check = ($event.target as HTMLInputElement).value || null" />
          <label class="text-muted-foreground" for="d-inv">{{ t("designer.invisible") }}</label>
          <input id="d-inv" v-model="col.invisible" type="checkbox" class="justify-self-start" />
          <template v-if="def.versioned">
            <label class="text-muted-foreground" for="d-unv" :title="t('designer.unversionedHint')">{{ t("designer.unversioned") }}</label>
            <input id="d-unv" v-model="col.unversioned" type="checkbox" class="justify-self-start" :title="t('designer.unversionedHint')" />
          </template>
        </fieldset>
      </template>

      <template v-else-if="section === 'indexes'">
        <table class="w-full border-collapse">
          <thead class="text-muted-foreground">
            <tr>
              <th :class="head">{{ t("designer.kind") }}</th>
              <th :class="head">{{ t("sequence.name") }}</th>
              <th :class="head">{{ t("designer.indexColumns") }}</th>
              <th :class="head">{{ t("structure.comment") }}</th>
              <th v-if="canIgnore" :class="head" :title="t('designer.ignoredHint')">{{ t("designer.ignored") }}</th>
              <th :class="head" />
            </tr>
          </thead>
          <tbody>
            <tr v-for="(x, i) in def.indexes" :key="i">
              <td :class="cell">
                <select v-model="x.kind" :aria-label="t('designer.kind')" :class="cn(input, 'w-28 font-sans')" @change="x.kind === 'PRIMARY' && ((x.name = 'PRIMARY'), (x.ignored = false))">
                  <option v-for="k in KINDS" :key="k" :value="k">{{ k }}</option>
                </select>
              </td>
              <td :class="cell"><input v-model="x.name" :disabled="x.kind === 'PRIMARY'" :placeholder="t('designer.autoName')" :aria-label="t('structure.name')" :class="input" /></td>
              <td :class="cell">
                <input :value="indexCols(x)" placeholder="a, b(10) DESC" :aria-label="t('designer.indexColumns')" :class="input" @change="setIndexCols(x, ($event.target as HTMLInputElement).value)" />
                <select
                  v-if="(x.kind === 'PRIMARY' || x.kind === 'UNIQUE') && appPeriods.length"
                  :value="x.overlaps ?? ''"
                  :aria-label="t('designer.overlaps')"
                  :class="cn(input, 'mt-1 w-56 font-sans')"
                  @change="x.overlaps = ($event.target as HTMLSelectElement).value || null"
                >
                  <option value="">{{ t("designer.noOverlaps") }}</option>
                  <option v-for="p in appPeriods" :key="p.name" :value="p.name">{{ t("designer.overlapsOn", { name: p.name }) }}</option>
                </select>
                <!-- VECTOR options: empty = server default (mhnsw_default_m / mhnsw_default_distance). -->
                <div v-if="x.kind === 'VECTOR'" class="mt-1 flex items-center gap-2 font-sans">
                  <select v-model="x.distance" :aria-label="t('vector.distance')" :class="cn(input, 'w-32')">
                    <option :value="null">{{ t("vector.distance") }} : {{ t("vector.serverDefault") }}</option>
                    <option value="euclidean">{{ t("vector.euclidean") }}</option>
                    <option value="cosine">{{ t("vector.cosine") }}</option>
                  </select>
                  <input
                    :value="x.m ?? ''"
                    type="number"
                    min="3"
                    max="200"
                    :placeholder="t('vector.m')"
                    :title="t('vector.mHint')"
                    :aria-label="t('vector.mHint')"
                    :class="cn(input, 'w-16')"
                    @change="x.m = ($event.target as HTMLInputElement).value ? Number(($event.target as HTMLInputElement).value) : null"
                  />
                </div>
              </td>
              <td :class="cell"><input v-model="x.comment" :aria-label="t('structure.comment')" :class="cn(input, 'font-sans')" /></td>
              <td v-if="canIgnore" :class="cn(cell, 'text-center')"><input v-model="x.ignored" type="checkbox" :disabled="x.kind === 'PRIMARY'" :aria-label="t('designer.ignored')" /></td>
              <td :class="cell"><button :class="icon" :title="t('designer.remove')" @click="def.indexes.splice(i, 1)"><Trash2 class="size-3.5" /><span class="sr-only">{{ t("designer.remove") }}</span></button></td>
            </tr>
          </tbody>
        </table>
        <button :class="cn(btn, 'mt-1')" @click="def.indexes.push({ original: null, name: '', kind: 'INDEX', columns: [], comment: '', ignored: false, m: null, distance: null })">
          <Plus class="size-3.5" />{{ t("designer.addIndex") }}
        </button>
      </template>

      <template v-else-if="section === 'fks'">
        <table class="w-full border-collapse">
          <thead class="text-muted-foreground">
            <tr>
              <th :class="head">{{ t("sequence.name") }}</th>
              <th :class="head">{{ t("designer.indexColumns") }}</th>
              <th :class="head">{{ t("designer.refSchema") }}</th>
              <th :class="head">{{ t("designer.refTable") }}</th>
              <th :class="head">{{ t("designer.refColumns") }}</th>
              <th :class="head">{{ t("designer.onDelete") }}</th>
              <th :class="head">{{ t("designer.onUpdateFk") }}</th>
              <th :class="head" />
            </tr>
          </thead>
          <tbody>
            <tr v-for="(f, i) in def.fks" :key="i">
              <td :class="cell"><input v-model="f.name" :placeholder="t('designer.autoName')" :aria-label="t('structure.name')" :class="input" /></td>
              <td :class="cell"><input :value="list(f.columns)" :aria-label="t('designer.indexColumns')" :class="input" @change="f.columns = parseList(($event.target as HTMLInputElement).value)" /></td>
              <td :class="cell"><input v-model="f.refSchema" list="designer-schemas" :placeholder="schema" :aria-label="t('designer.refSchema')" :class="input" /></td>
              <td :class="cell">
                <input v-model="f.refTable" :list="`designer-fk-tables-${i}`" :aria-label="t('designer.refTable')" :class="input" @focus="loadTables(fkSchema(f.refSchema))" />
                <datalist :id="`designer-fk-tables-${i}`"><option v-for="tb in refTables[fkSchema(f.refSchema)] ?? []" :key="tb" :value="tb" /></datalist>
              </td>
              <td :class="cell">
                <input
                  :value="list(f.refColumns)"
                  :list="`designer-fk-cols-${i}`"
                  :aria-label="t('designer.refColumns')"
                  :class="input"
                  @focus="loadCols(fkSchema(f.refSchema), f.refTable)"
                  @change="f.refColumns = parseList(($event.target as HTMLInputElement).value)"
                />
                <datalist :id="`designer-fk-cols-${i}`"><option v-for="c in refCols[`${fkSchema(f.refSchema)}.${f.refTable}`] ?? []" :key="c" :value="c" /></datalist>
              </td>
              <td :class="cell">
                <select v-model="f.onDelete" :aria-label="t('designer.onDelete')" :class="cn(input, 'font-sans')"><option v-for="r in RULES" :key="r">{{ r }}</option></select>
              </td>
              <td :class="cell">
                <select v-model="f.onUpdate" :aria-label="t('designer.onUpdateFk')" :class="cn(input, 'font-sans')"><option v-for="r in RULES" :key="r">{{ r }}</option></select>
              </td>
              <td :class="cell"><button :class="icon" :title="t('designer.remove')" @click="def.fks.splice(i, 1)"><Trash2 class="size-3.5" /><span class="sr-only">{{ t("designer.remove") }}</span></button></td>
            </tr>
          </tbody>
        </table>
        <button
          :class="cn(btn, 'mt-1')"
          @click="def.fks.push({ original: null, name: '', columns: [], refSchema: '', refTable: '', refColumns: [], onUpdate: 'RESTRICT', onDelete: 'RESTRICT' })"
        >
          <Plus class="size-3.5" />{{ t("designer.addFk") }}
        </button>
      </template>

      <template v-else-if="section === 'checks'">
        <table class="w-full border-collapse">
          <thead class="text-muted-foreground">
            <tr>
              <th :class="cn(head, 'w-56')">{{ t("sequence.name") }}</th>
              <th :class="head">{{ t("designer.condition") }}</th>
              <th :class="head" />
            </tr>
          </thead>
          <tbody>
            <tr v-for="(c, i) in def.checks" :key="i">
              <td :class="cell"><input v-model="c.name" :placeholder="t('designer.autoName')" :aria-label="t('structure.name')" :class="input" /></td>
              <td :class="cell"><input v-model="c.clause" placeholder="price >= 0" :aria-label="t('designer.condition')" :class="input" /></td>
              <td :class="cell"><button :class="icon" :title="t('designer.remove')" @click="def.checks.splice(i, 1)"><Trash2 class="size-3.5" /><span class="sr-only">{{ t("designer.remove") }}</span></button></td>
            </tr>
          </tbody>
        </table>
        <button :class="cn(btn, 'mt-1')" @click="def.checks.push({ original: null, name: '', clause: '' })"><Plus class="size-3.5" />{{ t("designer.addCheck") }}</button>
      </template>

      <div v-else class="grid max-w-2xl grid-cols-[11rem_1fr] items-center gap-x-3 gap-y-2">
        <label class="text-muted-foreground" for="d-name">{{ t("designer.tableName") }}</label>
        <input id="d-name" v-model="def.name" required :class="input" />
        <label class="text-muted-foreground" for="d-engine">{{ t("designer.engine") }}</label>
        <select id="d-engine" v-model="def.engine" :class="cn(input, 'font-sans')">
          <option v-for="e in engines" :key="e">{{ e }}</option>
          <option v-if="def.engine && !engines.includes(def.engine)">{{ def.engine }}</option>
        </select>
        <label class="text-muted-foreground" for="d-tcoll">{{ t("designer.collation") }}</label>
        <input id="d-tcoll" v-model="def.collation" list="designer-collations" :placeholder="t('designer.serverDefault')" :class="input" />
        <label class="text-muted-foreground" for="d-tcomment">{{ t("structure.comment") }}</label>
        <input id="d-tcomment" v-model="def.comment" :class="cn(input, 'font-sans')" />
        <label class="self-start pt-1 text-muted-foreground" for="d-part">{{ t("designer.partitions") }}</label>
        <div class="flex flex-col gap-1">
          <textarea
            id="d-part"
            v-model="def.partitions"
            rows="4"
            spellcheck="false"
            placeholder="PARTITION BY HASH (`id`) PARTITIONS 4"
            :class="cn(input, 'h-auto py-1 font-mono text-xs')"
          />
          <p class="text-muted-foreground">{{ t("designer.partitionsHint") }}</p>
        </div>
        <template v-if="mariadb">
          <label class="text-muted-foreground" for="d-vers">{{ t("designer.versioned") }}</label>
          <label class="flex items-center gap-2"><input id="d-vers" v-model="def.versioned" type="checkbox" /><span class="text-muted-foreground">{{ t("designer.versionedHint") }}</span></label>
          <span class="self-start pt-1 text-muted-foreground">{{ t("designer.periods") }}</span>
          <div class="flex flex-col gap-1">
            <div v-for="p in appPeriods" :key="p.name" class="flex items-center gap-1">
              <span class="w-32 truncate font-mono text-xs">{{ p.name }}</span>
              <select v-model="p.start" :aria-label="t('designer.periodStart')" :class="cn(input, 'w-36')">
                <option v-for="c in dateCols" :key="c">{{ c }}</option>
              </select>
              <select v-model="p.end" :aria-label="t('designer.periodEnd')" :class="cn(input, 'w-36')">
                <option v-for="c in dateCols" :key="c">{{ c }}</option>
              </select>
              <button :class="icon" :title="t('designer.remove')" @click="removePeriod(p.name)"><Trash2 class="size-3.5" /><span class="sr-only">{{ t("designer.remove") }}</span></button>
            </div>
            <form v-if="dateCols.length >= 2" class="flex items-center gap-1" @submit.prevent="addPeriod">
              <input v-model="periodName" required :placeholder="t('designer.periodName')" :aria-label="t('designer.periodName')" :class="cn(input, 'w-32')" />
              <button type="submit" :class="btn"><Plus class="size-3.5" />{{ t("designer.addPeriod") }}</button>
            </form>
            <p v-else class="text-muted-foreground">{{ t("designer.periodsNeedDates") }}</p>
          </div>
        </template>
      </div>
    </div>

    <datalist id="designer-types"><option v-for="ty in TYPES" :key="ty" :value="ty" /></datalist>
    <datalist id="designer-collations"><option v-for="c in collations" :key="c" :value="c" /></datalist>
    <datalist id="designer-schemas"><option v-for="s in schemas" :key="s" :value="s" /></datalist>

    <dialog ref="dialog" class="m-auto w-[min(56rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <div class="border-b px-4 py-2 font-semibold">{{ t("grid.previewTitle") }}</div>
      <pre class="max-h-[50vh] overflow-auto p-4 font-mono text-xs whitespace-pre-wrap select-text">{{ preview.join(";\n\n") }};</pre>
      <p v-if="old" class="px-4 text-muted-foreground">{{ t(`designer.algo_${algorithm}`) }}</p>
      <label v-if="old && mariadb" class="flex items-center gap-2 px-4 pt-2 text-muted-foreground">
        {{ t("designer.lockWait") }}
        <select :value="wait ?? ''" :disabled="busy" :class="cn(input, 'w-48 font-sans')" @change="(wait = ($event.target as HTMLSelectElement).value === '' ? null : Number(($event.target as HTMLSelectElement).value)), openPreview(algorithm)">
          <option v-for="w in WAITS" :key="String(w)" :value="w ?? ''">{{ w === null ? t("designer.lockWaitDefault") : w === 0 ? t("designer.lockNoWait") : t("designer.lockWaitSeconds", { n: w }) }}</option>
        </select>
      </label>
      <div v-if="busy && progress" class="flex items-center gap-2 px-4 pt-2">
        <div class="h-1.5 flex-1 overflow-hidden rounded-full bg-secondary"><div class="h-full bg-primary" :style="{ width: `${progress[0]}%` }" /></div>
        <span class="shrink-0 text-muted-foreground">{{ progress[2] ? t("designer.progress", { pct: progress[0].toFixed(1), stage: progress[1], stages: progress[2] }) : t("designer.progressPct", { pct: progress[0].toFixed(1) }) }}</span>
      </div>
      <p class="px-4 text-muted-foreground">{{ t("sequence.ddlHint") }}</p>
      <ErrorLine v-if="applyError" :error="applyError" class="mx-4 mt-2" />
      <div class="flex justify-end gap-2 px-4 py-3">
        <button v-if="busy" class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="api.cancelJob(sessionId, jobId)">{{ t("designer.stop") }}</button>
        <button v-else class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="dialog?.close()">{{ t("grid.cancel") }}</button>
        <!-- The server refused this algorithm: offer the next, slower one. -->
        <button
          v-if="applyError?.code === 'ALTER_ALGORITHM' && NEXT[algorithm]"
          class="h-7 rounded-sm border px-3 font-semibold hover:bg-secondary"
          @click="openPreview(NEXT[algorithm]!)"
        >
          {{ t(`designer.try_${NEXT[algorithm]}`) }}
        </button>
        <button v-else :disabled="busy" class="h-7 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50" @click="apply">{{ t("grid.apply") }}</button>
      </div>
    </dialog>
  </div>
</template>
