<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { FileCode, GitCompare, History, Loader2, Plus, Scissors, Trash2 } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type TableInfo, type VersionDiff, type VersioningAction, type VersioningState } from "@/lib/api";
import { cn } from "@/lib/utils";

/**
 * Temporal features of one table (MariaDB only): system versioning on/off, history purge,
 * SYSTEM_TIME partitions, comparison of two moments, application-time periods.
 */
const props = defineProps<{ sessionId: string; schema: string; table: string }>();
const emit = defineEmits<{ changed: [versioned: boolean]; editSql: [sql: string] }>();
const { t, locale } = useI18n();

const st = ref<VersioningState | null>(null);
const info = ref<TableInfo | null>(null);
const error = ref<AppError | null>(null);

async function load() {
  error.value = null;
  try {
    [st.value, info.value] = await Promise.all([
      api.versioningState(props.sessionId, props.schema, props.table),
      api.tableInfo(props.sessionId, props.schema, props.table),
    ]);
  } catch (e) {
    error.value = toAppError(e);
  }
}
onMounted(load);

/** <input type="datetime-local"> (local time) → UTC, the way the backend reads it. */
const utc = (local: string) => new Date(local).toISOString().slice(0, 19).replace("T", " ");
/** UTC timestamp from the server → local display. */
const local = (v: string | null) => (v ? new Date(v.replace(" ", "T") + "Z").toLocaleString(locale.value) : "—");
const num = (n: number) => n.toLocaleString(locale.value);

// Every action: preview the statements, then run them. Actions that destroy history ask for the table name.
const dialog = ref<HTMLDialogElement | null>(null);
const pending = ref<VersioningAction | null>(null);
const preview = ref<string[]>([]);
const typed = ref("");
const busy = ref(false);
const actionError = ref<AppError | null>(null);
const destructive = computed(() => !!pending.value && ["disable", "purge", "dropPartition"].includes(pending.value.kind));

async function ask(a: VersioningAction) {
  error.value = null;
  try {
    preview.value = await api.versioningSql(props.sessionId, props.schema, props.table, a);
    pending.value = a;
    typed.value = "";
    actionError.value = null;
    dialog.value?.showModal();
  } catch (e) {
    error.value = toAppError(e);
  }
}

async function run() {
  if (!pending.value) return;
  busy.value = true;
  actionError.value = null;
  try {
    await api.versioningApply(props.sessionId, props.schema, props.table, pending.value);
    dialog.value?.close();
    const was = st.value?.versioned;
    await load();
    diff.value = null;
    if (st.value && st.value.versioned !== was) emit("changed", st.value.versioned);
  } catch (e) {
    actionError.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

// Purge.
const purgeBefore = ref("");

// Partitions.
const part = ref({ interval: true, n: 1, unit: "MONTH", auto: true, partitions: 2 });
const UNITS = ["HOUR", "DAY", "WEEK", "MONTH", "YEAR"];
/** The server keeps at least one history partition: the last one cannot be dropped. */
const historyParts = computed(() => (st.value?.partitions ?? []).filter((p) => p.description !== "CURRENT").length);
const partitionAction = (): VersioningAction => ({
  kind: "partition",
  ...part.value,
  n: Number(part.value.n),
  partitions: part.value.auto ? null : Number(part.value.partitions),
});

// Compare two moments.
const fromAt = ref("");
const toAt = ref("");
const diff = ref<VersionDiff | null>(null);
const comparing = ref(false);
async function compare() {
  comparing.value = true;
  error.value = null;
  try {
    diff.value = await api.versioningCompare(props.sessionId, props.schema, props.table, info.value?.primaryKey ?? [], utc(fromAt.value), utc(toAt.value));
  } catch (e) {
    error.value = toAppError(e);
  } finally {
    comparing.value = false;
  }
}
/** Shown for each changed row: only the columns whose value differs. */
const changedCols = (r: VersionDiff["rows"][number]) => (r.before && r.after ? diff.value!.columns.map((_, i) => i).filter((i) => r.before![i] !== r.after![i]) : []);
const keyText = (r: VersionDiff["rows"][number]) => {
  const row = (r.after ?? r.before)!;
  return (info.value?.primaryKey ?? []).map((k) => row[diff.value!.columns.indexOf(k)] ?? "NULL").join(", ");
};
const cellText = (v: string | null | undefined) => (v == null ? "NULL" : v);

// Application-time periods.
const DATES = /^(date|datetime|timestamp)$/i;
const dateCols = computed(() => (info.value?.columns ?? []).filter((c) => DATES.test(c.dataType)).map((c) => c.name));
const period = ref({ name: "", start: "", end: "" });
async function portion(name: string) {
  emit("editSql", await api.portionSql(props.schema, props.table, name));
}

const h = "mb-2 flex items-center gap-1.5 font-semibold";
const card = "rounded-md border p-3";
const btn = "flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary disabled:opacity-50";
const primary = "flex h-7 items-center gap-1.5 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50";
const field = "h-7 rounded-sm border bg-background px-2";
const cell = "border-b px-2 py-1 text-left align-top";
</script>

<template>
  <!-- relative: keeps the absolute sr-only labels inside the scroll area (else they stretch the window). -->
  <div class="relative min-h-0 overflow-auto p-4">
    <ErrorLine v-if="error" :error="error" class="mb-3" />
    <p v-if="!st && !error" class="flex items-center gap-1.5 text-muted-foreground"><Loader2 class="size-3.5 animate-spin" />{{ t("versioning.loading") }}</p>

    <div v-if="st" class="flex max-w-4xl flex-col gap-4">
      <!-- System versioning -->
      <section :class="card">
        <h3 :class="h"><History class="size-4 text-primary" />{{ t("versioning.system") }}</h3>
        <template v-if="st.versioned">
          <dl class="grid grid-cols-[12rem_1fr] gap-x-3 gap-y-1">
            <dt class="text-muted-foreground">{{ t("versioning.columns") }}</dt>
            <dd class="font-mono">{{ st.system?.start }}, {{ st.system?.end }} <span class="font-sans text-muted-foreground">({{ st.implicit ? t("versioning.implicit") : t("versioning.explicit") }})</span></dd>
            <dt class="text-muted-foreground">{{ t("versioning.historyRows") }}</dt>
            <dd>{{ num(st.historyRows) }}</dd>
            <dt class="text-muted-foreground">{{ t("versioning.oldest") }}</dt>
            <dd>{{ local(st.oldest) }}</dd>
          </dl>
          <button :class="cn(btn, 'mt-3 hover:text-destructive')" @click="ask({ kind: 'disable' })"><Trash2 class="size-3.5" />{{ t("versioning.disable") }}</button>
        </template>
        <template v-else>
          <p class="text-muted-foreground">{{ t("versioning.off") }}</p>
          <button :class="cn(primary, 'mt-3')" @click="ask({ kind: 'enable' })"><History class="size-3.5" />{{ t("versioning.enable") }}</button>
        </template>
      </section>

      <template v-if="st.versioned">
        <!-- Purge -->
        <section :class="card">
          <h3 :class="h"><Scissors class="size-4 text-primary" />{{ t("versioning.purge") }}</h3>
          <p class="mb-2 text-muted-foreground">{{ t("versioning.purgeHint") }}</p>
          <div class="flex flex-wrap items-center gap-2">
            <label class="flex items-center gap-1.5">{{ t("versioning.before") }}<input v-model="purgeBefore" type="datetime-local" step="1" :class="field" /></label>
            <button :disabled="!purgeBefore" :class="btn" @click="ask({ kind: 'purge', before: utc(purgeBefore) })">{{ t("versioning.purgeRun") }}</button>
          </div>
        </section>

        <!-- Partitions -->
        <section :class="card">
          <h3 :class="h">{{ t("versioning.partitions") }}</h3>
          <p v-if="st.otherPartitioning" class="text-warning">{{ t("versioning.otherPartitioning") }}</p>
          <template v-else>
            <table v-if="st.partitions.length" class="mb-3 w-full border-collapse">
              <thead class="text-muted-foreground">
                <tr>
                  <th :class="cell">{{ t("versioning.partition") }}</th>
                  <th :class="cell">{{ t("versioning.upTo") }}</th>
                  <th :class="cell">{{ t("versioning.rows") }}</th>
                  <th :class="cell" />
                </tr>
              </thead>
              <tbody>
                <tr v-for="p in st.partitions" :key="p.name">
                  <td :class="cn(cell, 'font-mono')">{{ p.name }}</td>
                  <td :class="cell">{{ p.description === "CURRENT" ? t("versioning.current") : p.description ? local(p.description) : "—" }}</td>
                  <td :class="cell">{{ num(p.rows) }}</td>
                  <td :class="cn(cell, 'text-right')">
                    <button v-if="p.description !== 'CURRENT' && historyParts > 1" :title="t('versioning.dropPartition')" class="text-muted-foreground hover:text-destructive" @click="ask({ kind: 'dropPartition', name: p.name })">
                      <Trash2 class="size-3.5" /><span class="sr-only">{{ t("versioning.dropPartition") }}</span>
                    </button>
                  </td>
                </tr>
              </tbody>
            </table>
            <p v-else class="mb-2 text-muted-foreground">{{ t("versioning.noPartitions") }}</p>
            <div class="flex flex-wrap items-center gap-3">
              <label class="flex items-center gap-1.5"><input v-model="part.interval" type="radio" :value="true" class="size-3.5" />{{ t("versioning.byInterval") }}</label>
              <label class="flex items-center gap-1.5"><input v-model="part.interval" type="radio" :value="false" class="size-3.5" />{{ t("versioning.byLimit") }}</label>
              <input v-model.number="part.n" type="number" min="1" :aria-label="t('versioning.amount')" :class="cn(field, 'w-24')" />
              <select v-if="part.interval" v-model="part.unit" :aria-label="t('versioning.unit')" :class="field">
                <option v-for="u in UNITS" :key="u" :value="u">{{ t(`versioning.units.${u}`) }}</option>
              </select>
              <span v-else class="text-muted-foreground">{{ t("versioning.rowsPerPartition") }}</span>
            </div>
            <div class="mt-2 flex flex-wrap items-center gap-3">
              <label class="flex items-center gap-1.5" :title="t('versioning.autoHint')"><input v-model="part.auto" type="checkbox" class="size-3.5" />{{ t("versioning.auto") }}</label>
              <label v-if="!part.auto" class="flex items-center gap-1.5">{{ t("versioning.count") }}<input v-model.number="part.partitions" type="number" min="2" :class="cn(field, 'w-20')" /></label>
              <button :disabled="!(part.n >= 1)" :class="btn" @click="ask(partitionAction())">{{ st.partitions.length ? t("versioning.repartition") : t("versioning.partitionRun") }}</button>
              <button v-if="st.partitions.length" :class="btn" @click="ask({ kind: 'unpartition' })">{{ t("versioning.unpartition") }}</button>
            </div>
          </template>
        </section>

        <!-- Compare -->
        <section :class="card">
          <h3 :class="h"><GitCompare class="size-4 text-primary" />{{ t("versioning.compare") }}</h3>
          <p v-if="!info?.primaryKey.length" class="text-muted-foreground">{{ t("versioning.compareNoKey") }}</p>
          <template v-else>
            <div class="flex flex-wrap items-center gap-2">
              <label class="flex items-center gap-1.5">{{ t("versioning.from") }}<input v-model="fromAt" type="datetime-local" step="1" :class="field" /></label>
              <label class="flex items-center gap-1.5">{{ t("versioning.to") }}<input v-model="toAt" type="datetime-local" step="1" :class="field" /></label>
              <button :disabled="!fromAt || !toAt || comparing" :class="btn" @click="compare">
                <Loader2 v-if="comparing" class="size-3.5 animate-spin" />{{ t("versioning.compareRun") }}
              </button>
            </div>
            <template v-if="diff">
              <p class="mt-2 text-muted-foreground">
                {{ t("versioning.diffCount", diff.rows.length) }}
                <span v-if="diff.truncated" class="text-warning">· {{ t("versioning.diffTruncated") }}</span>
              </p>
              <table v-if="diff.rows.length" class="mt-2 w-full border-collapse">
                <thead class="text-muted-foreground">
                  <tr>
                    <th :class="cell">{{ t("versioning.change") }}</th>
                    <th :class="cell">{{ info.primaryKey.join(", ") }}</th>
                    <th :class="cell">{{ t("versioning.details") }}</th>
                  </tr>
                </thead>
                <tbody>
                  <tr v-for="(r, i) in diff.rows.slice(0, 1000)" :key="i">
                    <td :class="cn(cell, r.kind === 'added' ? 'text-success' : r.kind === 'removed' ? 'text-destructive' : 'text-warning')">{{ t(`versioning.kinds.${r.kind}`) }}</td>
                    <td :class="cn(cell, 'font-mono')">{{ keyText(r) }}</td>
                    <td :class="cn(cell, 'font-mono text-xs')">
                      <template v-if="r.kind === 'changed'">
                        <div v-for="c in changedCols(r)" :key="c">
                          <span class="text-muted-foreground">{{ diff.columns[c] }}</span> {{ cellText(r.before![c]) }} → {{ cellText(r.after![c]) }}
                        </div>
                      </template>
                      <span v-else class="break-all text-muted-foreground">{{ (r.after ?? r.before)!.map(cellText).join(" · ") }}</span>
                    </td>
                  </tr>
                </tbody>
              </table>
              <p v-if="diff.rows.length > 1000" class="mt-1 text-muted-foreground">{{ t("versioning.diffShown") }}</p>
            </template>
          </template>
        </section>
      </template>

      <!-- Application-time periods -->
      <section :class="card">
        <h3 :class="h">{{ t("versioning.appPeriods") }}</h3>
        <p class="mb-2 text-muted-foreground">{{ t("versioning.appPeriodsHint") }}</p>
        <ul v-if="st.periods.length" class="mb-3 flex flex-col gap-1">
          <li v-for="p in st.periods" :key="p.name" class="flex items-center gap-2">
            <span class="font-mono font-semibold">{{ p.name }}</span>
            <span class="font-mono text-muted-foreground">({{ p.start }}, {{ p.end }})</span>
            <button :class="cn(btn, 'ml-auto h-6 px-2')" @click="portion(p.name)"><FileCode class="size-3.5" />{{ t("versioning.portion") }}</button>
            <button :title="t('versioning.dropPeriod')" class="text-muted-foreground hover:text-destructive" @click="ask({ kind: 'dropPeriod', name: p.name })">
              <Trash2 class="size-3.5" /><span class="sr-only">{{ t("versioning.dropPeriod") }}</span>
            </button>
          </li>
        </ul>
        <p v-if="st.periods.length" class="mb-3 text-muted-foreground">{{ t("versioning.portionHint") }}</p>
        <p v-if="dateCols.length < 2" class="text-muted-foreground">{{ t("versioning.needDates") }}</p>
        <div v-else class="flex flex-wrap items-center gap-2">
          <input v-model="period.name" :placeholder="t('versioning.periodName')" :aria-label="t('versioning.periodName')" spellcheck="false" :class="cn(field, 'w-36 font-mono')" />
          <select v-model="period.start" :aria-label="t('versioning.start')" :class="field">
            <option value="" disabled>{{ t("versioning.start") }}</option>
            <option v-for="c in dateCols" :key="c" :value="c">{{ c }}</option>
          </select>
          <select v-model="period.end" :aria-label="t('versioning.end')" :class="field">
            <option value="" disabled>{{ t("versioning.end") }}</option>
            <option v-for="c in dateCols" :key="c" :value="c">{{ c }}</option>
          </select>
          <button :disabled="!period.name.trim() || !period.start || !period.end || period.start === period.end" :class="btn" @click="ask({ kind: 'addPeriod', ...period })">
            <Plus class="size-3.5" />{{ t("versioning.addPeriod") }}
          </button>
        </div>
      </section>
    </div>

    <dialog ref="dialog" class="m-auto w-[min(48rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <div :class="cn('border-b px-4 py-2 font-semibold', destructive && 'text-destructive')">{{ t("versioning.previewTitle") }}</div>
      <pre class="max-h-[40vh] overflow-auto p-4 font-mono text-xs whitespace-pre-wrap select-text">{{ preview.join(";\n") }};</pre>
      <p class="px-4 text-muted-foreground">{{ destructive ? t("versioning.destructiveHint") : t("versioning.previewHint") }}</p>
      <label v-if="destructive" class="mx-4 mt-2 flex flex-col gap-1">
        {{ t("object.typeName", { name: table }) }}
        <input v-model="typed" spellcheck="false" class="h-7 rounded-sm border bg-background px-2 font-mono" />
      </label>
      <ErrorLine v-if="actionError" :error="actionError" class="mx-4 mt-2" />
      <div class="flex justify-end gap-2 px-4 py-3">
        <button class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="dialog?.close()">{{ t("grid.cancel") }}</button>
        <button
          :disabled="busy || (destructive && typed !== table)"
          :class="cn('h-7 rounded-sm px-3 font-semibold text-primary-foreground disabled:opacity-50', destructive ? 'bg-destructive' : 'bg-primary')"
          @click="run"
        >
          {{ t("grid.apply") }}
        </button>
      </div>
    </dialog>
  </div>
</template>
