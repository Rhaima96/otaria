<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { AgGridVue } from "ag-grid-vue3";
import type { ColDef, SelectionChangedEvent } from "ag-grid-community";
import { Loader2, RefreshCw, Trash2, Undo2 } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type BinlogEvent, type BinlogState } from "@/lib/api";
import { transactions, type Txn } from "@/lib/binlog";
import { theme } from "@/lib/grid";
import { cn } from "@/lib/utils";

/** Binary log reader (MariaDB only): files on the left, events of the chosen file, one page at a time. */
// active: unused here, passed to every server screen (the polling ones pause when hidden).
const props = defineProps<{ sessionId: string; active?: boolean }>();
const emit = defineEmits<{ editSql: [sql: string] }>();
const { t, locale } = useI18n();

const PAGE = 1000;
const st = ref<BinlogState | null>(null);
const file = ref("");
const events = ref<BinlogEvent[]>([]);
const more = ref(false);
const busy = ref(false);
const filter = ref("");
const error = ref<AppError | null>(null);
const selected = ref<BinlogEvent[]>([]);
const undoing = ref(false);
/** Byte position the page starts at (null = start of the file), and its input. */
const from = ref<number | null>(null);
const jumpTo = ref("");
/** One row per transaction (GTID) instead of one per event. */
const byTxn = ref(false);

async function load() {
  error.value = null;
  try {
    st.value = await api.binlogState(props.sessionId);
    // Start on the file being written (the last one), or stay on the chosen one if it still exists.
    const names = st.value.files.map((f) => f.name);
    const keep = names.includes(file.value);
    if (!keep) from.value = null;
    await open(keep ? file.value : (names.at(-1) ?? ""));
  } catch (e) {
    error.value = toAppError(e);
  }
}
onMounted(load);

async function open(name: string, append = false) {
  if (!name) return;
  busy.value = true;
  error.value = null;
  try {
    const page = await api.binlogEvents(props.sessionId, name, from.value, append ? events.value.length : 0, PAGE);
    file.value = name;
    events.value = append ? [...events.value, ...page] : page;
    more.value = page.length === PAGE;
  } catch (e) {
    error.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

function pickFile(name: string) {
  from.value = null;
  jumpTo.value = "";
  open(name);
}
async function jump() {
  const n = Number(jumpTo.value);
  const before = from.value;
  from.value = jumpTo.value.trim() && Number.isInteger(n) && n >= 4 ? n : null;
  await open(file.value);
  if (error.value) from.value = before; // refused position: the list shown is still the old one
  else if (from.value !== null && events.value.length) jumpTo.value = String((from.value = events.value[0].pos)); // moved back to its event's start
}

// Purge: every file before the chosen one goes; the preview names them and the file name must be retyped.
const purgeDialog = ref<HTMLDialogElement | null>(null);
const purge = ref<{ file: string; sql: string; gone: string[] } | null>(null);
const purgeTyped = ref("");
const purgeError = ref<AppError | null>(null);
async function askPurge(i: number) {
  const f = st.value!.files[i].name;
  purge.value = { file: f, sql: await api.binlogPurgeSql(f), gone: st.value!.files.slice(0, i).map((x) => x.name) };
  purgeTyped.value = "";
  purgeError.value = null;
  purgeDialog.value?.showModal();
}
async function runPurge() {
  busy.value = true;
  purgeError.value = null;
  try {
    await api.binlogPurge(props.sessionId, purge.value!.file);
    purgeDialog.value?.close();
    await load();
  } catch (e) {
    purgeError.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

/**
 * Range to undo: the whole transactions of the selected events, from the Gtid that opens the first one to the
 * end of the Xid (or COMMIT) that closes the last one. Row events report end_log_pos 0 on MariaDB 12, so an
 * event's end is taken from the next event's start.
 */
function range(): [number, number] {
  const ev = events.value;
  const at = (pos: number) => ev.findIndex((e) => e.pos === pos);
  let i = at(Math.min(...selected.value.map((e) => e.pos)));
  while (i > 0 && ev[i].kind !== "Gtid") i--;
  let j = at(Math.max(...selected.value.map((e) => e.pos)));
  const closes = (e: BinlogEvent) => e.kind === "Xid" || (e.kind === "Query" && /^\s*COMMIT/i.test(e.info));
  while (j < ev.length - 1 && !closes(ev[j])) j++;
  const end = ev[j].endPos > ev[j].pos ? ev[j].endPos : (ev[j + 1]?.pos ?? ev[j].endPos);
  return [ev[i].pos, end];
}

/** Undo SQL of the selected transactions, opened in the SQL editor (never run from here). */
async function flashback() {
  const [start, stop] = range();
  undoing.value = true;
  error.value = null;
  try {
    const sql = await api.binlogFlashback(props.sessionId, file.value, start, stop);
    emit("editSql", `-- ${t("binlog.undoHeader", { file: file.value, start, stop })}\n\n${sql}`);
  } catch (e) {
    error.value = toAppError(e);
  } finally {
    undoing.value = false;
  }
}

const shown = computed(() => {
  const f = filter.value.trim().toLowerCase();
  return f ? events.value.filter((e) => `${e.kind} ${e.info}`.toLowerCase().includes(f)) : events.value;
});

const txns = computed(() => {
  const f = filter.value.trim().toLowerCase();
  const all = transactions(events.value);
  return f ? all.filter((x) => `${x.gtid} ${x.tables.join(" ")} ${x.statements.join(" ")}`.toLowerCase().includes(f)) : all;
});
const txnDefs = computed<ColDef<Txn>[]>(() => [
  { field: "gtid", headerName: t("binlog.gtidCol"), width: 120 },
  { field: "pos", headerName: t("binlog.pos"), width: 100 },
  { headerName: t("binlog.events"), width: 90, valueGetter: (p) => p.data?.events.length },
  { headerName: t("binlog.tables"), width: 180, valueGetter: (p) => p.data?.tables.join(", ") },
  { headerName: t("binlog.statements"), flex: 1, valueGetter: (p) => p.data?.statements.join(" · "), tooltipValueGetter: (p) => p.data?.statements.join("\n") },
]);

const colDefs = computed<ColDef<BinlogEvent>[]>(() => [
  { field: "pos", headerName: t("binlog.pos"), width: 100 },
  { field: "endPos", headerName: t("binlog.endPos"), width: 100 },
  { field: "kind", headerName: t("binlog.kind"), width: 150 },
  { field: "serverId", headerName: t("binlog.serverId"), width: 90 },
  { field: "info", headerName: t("binlog.info"), flex: 1, tooltipField: "info" },
]);

const size = (n: number) => {
  const mb = n >= 1024 * 1024;
  return new Intl.NumberFormat(locale.value, { style: "unit", unit: mb ? "megabyte" : "kilobyte", maximumFractionDigits: 1 }).format(n / (mb ? 1024 * 1024 : 1024));
};
const btn = "flex h-6 items-center gap-1 rounded-sm px-2 text-muted-foreground hover:bg-secondary hover:text-foreground disabled:opacity-50";
</script>

<template>
  <div class="flex flex-col">
    <div class="flex h-9 shrink-0 items-center gap-3 border-b px-3">
      <input v-model="filter" :placeholder="t('binlog.filter')" :aria-label="t('binlog.filter')" class="h-6 w-64 rounded-sm border bg-background px-2" />
      <span v-if="st" class="text-muted-foreground">{{ t("binlog.format") }} <span class="font-mono text-foreground">{{ st.format }}</span></span>
      <span v-if="st?.gtid" class="text-muted-foreground">{{ t("binlog.gtid") }} <span class="font-mono text-foreground">{{ st.gtid }}</span></span>
      <label class="flex items-center gap-1.5 text-muted-foreground">
        <input :checked="byTxn" type="checkbox" class="size-3.5" @change="(byTxn = ($event.target as HTMLInputElement).checked), (selected = [])" />{{ t("binlog.byTxn") }}
      </label>
      <form class="flex items-center gap-1" @submit.prevent="jump">
        <input v-model="jumpTo" inputmode="numeric" :placeholder="t('binlog.jumpPlaceholder')" :aria-label="t('binlog.jumpPlaceholder')" class="h-6 w-28 rounded-sm border bg-background px-2 font-mono text-xs" />
        <button type="submit" :class="btn" :title="t('binlog.jumpHint')">{{ t("binlog.jump") }}</button>
      </form>
      <button
        :class="cn(btn, 'ml-auto')"
        :disabled="!selected.length || st?.format !== 'ROW' || undoing"
        :title="st && st.format !== 'ROW' ? t('binlog.undoNeedsRow') : t('binlog.undoHint')"
        @click="flashback"
      >
        <Loader2 v-if="undoing" class="size-3.5 animate-spin" /><Undo2 v-else class="size-3.5" />{{ t("binlog.undo", { n: selected.length }) }}
      </button>
      <button :class="btn" @click="load"><RefreshCw class="size-3.5" />{{ t("admin.refresh") }}</button>
    </div>

    <ErrorLine v-if="error" :error="error" class="m-3" />

    <dialog ref="purgeDialog" class="m-auto w-[min(36rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <form v-if="purge" @submit.prevent="runPurge">
        <div class="border-b px-4 py-2 font-semibold">{{ t("grid.previewTitle") }}</div>
        <pre class="p-4 font-mono text-xs whitespace-pre-wrap select-text">{{ purge.sql }};</pre>
        <p class="px-4 text-warning">{{ t("binlog.purgeWarning", { n: purge.gone.length }) }}</p>
        <p class="max-h-24 overflow-auto px-4 font-mono text-xs text-muted-foreground">{{ purge.gone.join(", ") }}</p>
        <label class="flex flex-col gap-1 px-4 pt-3">
          <span class="text-muted-foreground">{{ t("object.typeName", { name: purge.file }) }}</span>
          <input v-model="purgeTyped" autocomplete="off" spellcheck="false" class="h-7 rounded-sm border bg-background px-2 font-mono" />
        </label>
        <ErrorLine v-if="purgeError" :error="purgeError" class="mx-4 mt-2" />
        <div class="flex justify-end gap-2 px-4 py-3">
          <button type="button" class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="purgeDialog?.close()">{{ t("grid.cancel") }}</button>
          <button type="submit" :disabled="busy || purgeTyped !== purge.file" class="h-7 rounded-sm bg-destructive px-3 font-semibold text-primary-foreground disabled:opacity-50">
            {{ t("binlog.purgeRun") }}
          </button>
        </div>
      </form>
    </dialog>

    <div v-if="st" class="flex min-h-0 flex-1">
      <ul class="w-56 shrink-0 overflow-auto border-r py-1" :aria-label="t('binlog.files')">
        <li v-for="(f, i) in st.files" :key="f.name" :class="cn('group flex items-center hover:bg-secondary', f.name === file && 'bg-selection')">
          <button
            class="flex min-w-0 flex-1 items-center justify-between gap-2 py-1 pl-3 text-left"
            :title="i === st.files.length - 1 ? t('binlog.current') : undefined"
            @click="pickFile(f.name)"
          >
            <span :class="cn('truncate font-mono', i === st.files.length - 1 && 'text-primary')">{{ f.name }}</span>
            <span class="shrink-0 text-muted-foreground">{{ size(f.size) }}</span>
          </button>
          <button
            v-if="i > 0"
            class="mx-1 rounded-sm p-0.5 text-muted-foreground opacity-0 group-hover:opacity-100 hover:text-destructive focus-visible:opacity-100"
            :title="t('binlog.purge', { n: i })"
            :aria-label="t('binlog.purge', { n: i })"
            @click="askPurge(i)"
          >
            <Trash2 class="size-3.5" />
          </button>
          <span v-else class="w-6 shrink-0" />
        </li>
      </ul>

      <div class="flex min-w-0 flex-1 flex-col">
        <AgGridVue
          v-if="byTxn"
          class="min-h-0 flex-1"
          :theme="theme"
          :row-data="txns"
          :column-defs="txnDefs"
          :default-col-def="{ resizable: true, sortable: true }"
          :locale-text="{ noRowsToShow: t('grid.noRows') }"
          :enable-cell-text-selection="true"
          :tooltip-show-delay="300"
          :row-selection="{ mode: 'multiRow', checkboxes: false, headerCheckbox: false, enableClickSelection: true }"
          @selection-changed="(e: SelectionChangedEvent<Txn>) => (selected = e.api.getSelectedRows().flatMap((x) => x.events))"
        />
        <AgGridVue
          v-else
          class="min-h-0 flex-1"
          :theme="theme"
          :row-data="shown"
          :column-defs="colDefs"
          :default-col-def="{ resizable: true, sortable: true }"
          :locale-text="{ noRowsToShow: t('grid.noRows') }"
          :enable-cell-text-selection="true"
          :tooltip-show-delay="300"
          :row-selection="{ mode: 'multiRow', checkboxes: false, headerCheckbox: false, enableClickSelection: true }"
          @selection-changed="(e: SelectionChangedEvent<BinlogEvent>) => (selected = e.api.getSelectedRows())"
        />
        <div class="flex h-7 shrink-0 items-center gap-2 border-t px-2 text-muted-foreground">
          <Loader2 v-if="busy" class="size-3.5 animate-spin" />
          <span v-if="from !== null" class="shrink-0 font-mono">{{ t("binlog.fromPos", { pos: from }) }}</span>
          <span v-if="byTxn" class="shrink-0">{{ t("binlog.txnCount", { n: txns.length }) }}</span>
          <span class="shrink-0">{{ shown.length < events.length ? t("binlog.matching", { n: shown.length, total: events.length }) : t("binlog.loaded", { n: events.length }) }}</span>
          <button v-if="more" :disabled="busy" :class="btn" class="text-foreground" @click="open(file, true)">{{ t("binlog.more") }}</button>
          <span class="ml-auto min-w-0 truncate" :title="t('binlog.hint')">{{ t("binlog.hint") }}</span>
        </div>
      </div>
    </div>
  </div>
</template>
