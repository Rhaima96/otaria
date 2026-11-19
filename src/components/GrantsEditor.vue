<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { Plus, X } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type Column, type DbObject, type GrantTarget, type PrivChange, type Privilege } from "@/lib/api";
import { cn } from "@/lib/utils";

const props = defineProps<{ sessionId: string; user: string; host: string; isRole: boolean; privileges: Privilege[] }>();
const { t } = useI18n();

/** One editable row: a target and its ticked privileges. The global row is always there. */
interface Row {
  target: GrantTarget;
  privs: string[];
}
const GLOBAL: GrantTarget = { schema: null, table: null, column: null, routine: null };
const keyOf = (t: GrantTarget) => JSON.stringify([t.schema, t.table, t.column, t.routine ?? null]);

const loaded = ref<Map<string, Set<string>>>(new Map());
const rows = ref<Row[]>([]);
const error = ref<AppError | null>(null);

async function load() {
  error.value = null;
  try {
    const grants = await api.accountGrants(props.sessionId, props.user, props.host, props.isRole);
    loaded.value = new Map(grants.map((g) => [keyOf(g.target), new Set(g.privileges)]));
    rows.value = grants.map((g) => ({ target: g.target, privs: [...g.privileges] }));
    if (!rows.value.some((r) => keyOf(r.target) === keyOf(GLOBAL))) rows.value.unshift({ target: GLOBAL, privs: [] });
  } catch (e) {
    error.value = toAppError(e);
  }
}
onMounted(load);
watch(() => [props.user, props.host], load);

type Level = "global" | "schema" | "table" | "column" | "routine";
const levelOf = (t: GrantTarget): Level => (t.routine ? "routine" : t.column ? "column" : t.table ? "table" : t.schema ? "schema" : "global");
const choices = (t: GrantTarget) => props.privileges.filter((p) => p[levelOf(t)]).map((p) => p.name);
function label(g: GrantTarget) {
  if (!g.schema) return t("users.global");
  const q = (s: string) => `\`${s}\``;
  if (g.routine) return `${g.routine} ${q(g.schema)}.${q(g.table!)}`;
  return g.column ? `${q(g.schema)}.${q(g.table!)} (${q(g.column)})` : g.table ? `${q(g.schema)}.${q(g.table)}` : `${q(g.schema)}.*`;
}

// Add a target: pick the level, then database, table, column (loaded on demand).
const level = ref<Exclude<Level, "global">>("schema");
const schemas = ref<string[]>([]);
const tables = ref<DbObject[]>([]);
const columns = ref<Column[]>([]);
const pick = ref<GrantTarget>({ schema: null, table: null, column: null });
const routines = ref<DbObject[]>([]);

onMounted(async () => {
  try {
    schemas.value = await api.listSchemas(props.sessionId);
  } catch (e) {
    error.value = toAppError(e);
  }
});
watch(
  () => pick.value.schema,
  async (s) => {
    pick.value.table = null;
    const objs = s ? await api.listObjects(props.sessionId, s) : [];
    tables.value = objs.filter((o) => o.kind === "table" || o.kind === "view");
    routines.value = objs.filter((o) => o.kind === "procedure" || o.kind === "function");
  },
);
watch(level, () => (pick.value.table = null));
watch(
  () => pick.value.table,
  async (tb) => {
    pick.value.column = null;
    columns.value = tb && pick.value.schema && level.value !== "routine" ? (await api.tableInfo(props.sessionId, pick.value.schema, tb)).columns : [];
  },
);

const canAdd = computed(() => {
  const p = pick.value;
  const complete = !!p.schema && (level.value === "schema" || !!p.table) && (level.value !== "column" || !!p.column);
  return complete && !rows.value.some((r) => keyOf(r.target) === keyOf(target()));
});
function target(): GrantTarget {
  const p = pick.value;
  if (level.value === "routine") {
    // The routine select's value is "kind:name" (a procedure and a function may share a name).
    const [kind, ...name] = (p.table ?? "").split(":");
    return { schema: p.schema, table: name.join(":"), column: null, routine: kind.toUpperCase() as "PROCEDURE" | "FUNCTION" };
  }
  return { schema: p.schema, table: level.value === "schema" ? null : p.table, column: level.value === "column" ? p.column : null };
}
function add() {
  rows.value.push({ target: target(), privs: [] });
}

/** Ticked vs loaded, per target; a removed row revokes everything it had. */
const changes = computed<PrivChange[]>(() => {
  const now = new Map(rows.value.map((r) => [keyOf(r.target), r]));
  const out: PrivChange[] = [];
  for (const r of rows.value) {
    const had = loaded.value.get(keyOf(r.target)) ?? new Set<string>();
    const grant = r.privs.filter((p) => !had.has(p));
    const revoke = [...had].filter((p) => !r.privs.includes(p));
    if (grant.length || revoke.length) out.push({ target: r.target, grant, revoke });
  }
  for (const [k, had] of loaded.value) {
    if (!now.has(k)) {
      const [schema, table, column, routine] = JSON.parse(k);
      out.push({ target: { schema, table, column, routine }, grant: [], revoke: [...had] });
    }
  }
  return out;
});

// Preview, then apply.
const dialog = ref<HTMLDialogElement | null>(null);
const preview = ref<string[]>([]);
const applyError = ref<AppError | null>(null);
const busy = ref(false);

async function review() {
  error.value = null;
  applyError.value = null;
  try {
    preview.value = await api.privilegesSql(props.user, props.host, props.isRole, changes.value);
    dialog.value?.showModal();
  } catch (e) {
    error.value = toAppError(e);
  }
}

async function apply() {
  busy.value = true;
  applyError.value = null;
  try {
    await api.privilegesApply(props.sessionId, props.user, props.host, props.isRole, changes.value);
    dialog.value?.close();
    await load();
  } catch (e) {
    applyError.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

const input = "h-7 rounded-sm border bg-background px-2";
const btn = "flex h-6 items-center gap-1 rounded-sm px-2 text-muted-foreground hover:bg-secondary hover:text-foreground";
</script>

<template>
  <div class="flex flex-col">
    <div class="min-h-0 flex-1 space-y-3 overflow-auto p-4">
      <ErrorLine v-if="error" :error="error" />
      <section v-for="(r, i) in rows" :key="keyOf(r.target)" class="rounded-md border">
        <div class="flex items-center gap-2 border-b bg-sidebar px-3 py-1.5">
          <span class="font-mono text-xs font-semibold">{{ label(r.target) }}</span>
          <span class="text-xs text-muted-foreground">{{ t(`users.level.${levelOf(r.target)}`) }}</span>
          <button v-if="r.target.schema" :class="cn(btn, 'ml-auto')" :title="t('users.removeTarget')" @click="rows.splice(i, 1)">
            <X class="size-3.5" /><span class="sr-only">{{ t("users.removeTarget") }}</span>
          </button>
        </div>
        <div class="grid grid-cols-[repeat(auto-fill,minmax(13rem,1fr))] gap-x-4 gap-y-1 p-3">
          <label v-for="p in choices(r.target)" :key="p" class="flex items-center gap-1.5 font-mono text-xs">
            <input v-model="r.privs" type="checkbox" :value="p" class="size-3.5" />{{ p }}
          </label>
        </div>
      </section>

      <div class="flex flex-wrap items-center gap-2 rounded-md border border-dashed p-3">
        <select v-model="level" :aria-label="t('users.levelLabel')" :class="input">
          <option value="schema">{{ t("users.level.schema") }}</option>
          <option value="table">{{ t("users.level.table") }}</option>
          <option value="column">{{ t("users.level.column") }}</option>
          <option value="routine">{{ t("users.level.routine") }}</option>
        </select>
        <select v-model="pick.schema" :aria-label="t('users.level.schema')" :class="cn(input, 'font-mono')">
          <option :value="null" disabled>{{ t("users.pickSchema") }}</option>
          <option v-for="s in schemas" :key="s" :value="s">{{ s }}</option>
        </select>
        <select v-if="level === 'routine'" v-model="pick.table" :aria-label="t('users.level.routine')" :class="cn(input, 'font-mono')">
          <option :value="null" disabled>{{ t("users.pickRoutine") }}</option>
          <option v-for="o in routines" :key="o.kind + o.name" :value="`${o.kind}:${o.name}`">{{ o.name }} ({{ t(`kinds.${o.kind}`) }})</option>
        </select>
        <select v-if="level === 'table' || level === 'column'" v-model="pick.table" :aria-label="t('users.level.table')" :class="cn(input, 'font-mono')">
          <option :value="null" disabled>{{ t("users.pickTable") }}</option>
          <option v-for="o in tables" :key="o.name" :value="o.name">{{ o.name }}</option>
        </select>
        <select v-if="level === 'column'" v-model="pick.column" :aria-label="t('users.level.column')" :class="cn(input, 'font-mono')">
          <option :value="null" disabled>{{ t("users.pickColumn") }}</option>
          <option v-for="c in columns" :key="c.name" :value="c.name">{{ c.name }}</option>
        </select>
        <button :disabled="!canAdd" :class="cn(btn, 'disabled:opacity-40')" @click="add"><Plus class="size-3.5" />{{ t("users.addTarget") }}</button>
      </div>
    </div>
    <div class="flex shrink-0 items-center gap-3 border-t px-4 py-2">
      <button :disabled="!changes.length" class="h-7 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50" @click="review">{{ t("users.review") }}</button>
      <span class="text-muted-foreground">{{ t("users.privilegesHint") }}</span>
    </div>

    <dialog ref="dialog" class="m-auto w-[min(48rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <div class="border-b px-4 py-2 font-semibold">{{ t("grid.previewTitle") }}</div>
      <pre class="max-h-[40vh] overflow-auto p-4 font-mono text-xs whitespace-pre-wrap select-text">{{ preview.join(";\n") + ";" }}</pre>
      <ErrorLine v-if="applyError" :error="applyError" class="mx-4 mt-2" />
      <div class="flex justify-end gap-2 px-4 py-3">
        <button class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="dialog?.close()">{{ t("grid.cancel") }}</button>
        <button :disabled="busy" class="h-7 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50" @click="apply">{{ t("grid.apply") }}</button>
      </div>
    </dialog>
  </div>
</template>
