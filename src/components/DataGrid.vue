<script setup lang="ts">
import { computed, onMounted, ref, shallowRef } from "vue";
import { useI18n } from "vue-i18n";
import { AgGridVue } from "ag-grid-vue3";
import { type ColDef, type GridApi, type GridReadyEvent, type SortChangedEvent } from "ag-grid-community";
import { Ban, Braces, Filter, History, Plus, RefreshCw, Save, Trash2, Undo2, X } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import JsonDialog from "@/components/JsonDialog.vue";
import { theme, vectorText } from "@/lib/grid";
import { api, toAppError, type AppError, type Change, type History as RowHistory, type Pairs, type Sort, type TableInfo } from "@/lib/api";

const props = defineProps<{ sessionId: string; schema: string; table: string; info: TableInfo; versioned?: boolean }>();
const emit = defineEmits<{ editSql: [sql: string] }>();
const { t, locale } = useI18n();
const PAGE = 1000;

/** One grid row. `orig` = values as loaded (null for a new row); `undefined` in `v` = not set (server default). */
interface GridRow {
  id: number;
  v: (string | null | undefined)[];
  orig: (string | null)[] | null;
  deleted: boolean;
  /** Columns whose value came cut (long TEXT / BLOB): read-only until the whole value is loaded. */
  cut: number[];
}

const columns = ref<string[]>([]);
const rows = shallowRef<GridRow[]>([]);
const hasMore = ref(false);
const filter = ref("");
const order = ref<Sort[]>([]);
const error = ref<AppError | null>(null);
const busy = ref(false);
/** Bumped on every edit: rows are mutated in place by the grid. */
const tick = ref(0);
let nextId = 0;
let grid: GridApi<GridRow> | null = null;

// System-versioned tables: view the table "as of" a past moment (read-only), and the history of a row.
/** <input type="datetime-local"> value, local time; "" = current data. */
const asOfLocal = ref("");
/** Sent as UTC: the backend runs these queries with time_zone = '+00:00'. */
const asOf = computed(() => (asOfLocal.value ? new Date(asOfLocal.value).toISOString().slice(0, 23).replace("T", " ") : null));
const selected = ref(0);

// Edits need the full primary key in the result; tables without one are read-only (like HeidiSQL).
const pkIndex = computed(() => props.info.primaryKey.map((k) => columns.value.indexOf(k)));
const hasKey = computed(() => pkIndex.value.length > 0 && !pkIndex.value.includes(-1));
const editable = computed(() => hasKey.value && !asOf.value);
// VECTOR cells are shown as text but stored as bytes: edited in SQL (VEC_FromText), not in the grid.
const BINARY = /blob|binary|bit|geometry|point|linestring|polygon|vector/i;
const vectorCols = computed(() => new Set(props.info.columns.filter((c) => c.dataType === "vector").map((c) => c.name)));
const readOnlyCol = (name: string) => {
  const c = props.info.columns.find((c) => c.name === name);
  return !c || BINARY.test(c.dataType) || /GENERATED|PERSISTENT/i.test(c.extra);
};

/** Last row loaded: the next page starts after it (keyset), the primary key breaking ties. */
let lastRow: (string | null)[] | null = null;
// Values shown as text that would not compare back exactly: hex bytes, rounded floats, cells cut to
// CELL_CUT, ENUM/SET (sorted by their index, compared as text). Such a sort pages by offset.
const INEXACT = /blob|binary|bit|float|double|real|vector|geometry|^(text|mediumtext|longtext|json|enum|set)$/i;
const exact = (column: string) => {
  const c = props.info.columns.find((c) => c.name === column);
  return !!c && !INEXACT.test(c.dataType);
};

async function load(append = false) {
  busy.value = true;
  error.value = null;
  try {
    // Server-side sort, with the primary key as tie-breaker so pages are stable.
    const pk = props.info.primaryKey.filter((k) => !order.value.some((s) => s.column === k)).map((column) => ({ column, desc: false }));
    const sort = [...order.value, ...pk];
    const keyset = append && lastRow !== null && hasKey.value && sort.every((s) => exact(s.column));
    const res = await api.tableData(props.sessionId, props.schema, props.table, {
      filter: filter.value,
      order: sort,
      offset: append ? rows.value.filter((r) => r.orig).length : 0,
      limit: PAGE,
      asOf: asOf.value,
      after: keyset ? sort.map((s) => lastRow![columns.value.indexOf(s.column)] ?? null) : null,
    });
    lastRow = res.rows.at(-1) ?? (append ? lastRow : null);
    columns.value = res.columns;
    const loaded = res.rows.map((r, k) => ({ id: nextId++, v: [...r], orig: r, deleted: false, cut: res.cut?.[k] ?? [] }));
    rows.value = append ? [...rows.value, ...loaded] : loaded;
    hasMore.value = res.rows.length === PAGE;
    tick.value++;
  } catch (e) {
    error.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

const keyOf = (r: GridRow): Pairs => pkIndex.value.map((i) => [columns.value[i], r.orig![i]]);

const changes = computed<Change[]>(() => {
  void tick.value;
  return rows.value.flatMap((r): Change[] => {
    if (!r.orig) {
      if (r.deleted) return [];
      return [{ kind: "insert", values: r.v.flatMap((v, i): Pairs => (v === undefined ? [] : [[columns.value[i], v]])) }];
    }
    if (r.deleted) return [{ kind: "delete", key: keyOf(r) }];
    const set = r.v.flatMap((v, i): Pairs => (v !== r.orig![i] && v !== undefined ? [[columns.value[i], v]] : []));
    return set.length ? [{ kind: "update", key: keyOf(r), set }] : [];
  });
});
const dirty = computed(() => changes.value.length > 0);
// Read by SessionView before closing the tab; reload: F5, refused while edits are pending.
defineExpose({ dirty, reload: () => !dirty.value && !busy.value && (load(), true) });

const colDefs = computed<ColDef<GridRow>[]>(() =>
  columns.value.map((name, i) => ({
    colId: String(i),
    headerName: name,
    headerTooltip: props.info.columns.find((c) => c.name === name)?.columnType,
    valueGetter: (p) => p.data?.v[i],
    valueSetter: (p) => {
      p.data.v[i] = p.newValue;
      return true;
    },
    // The editor gives "" for an empty cell; NULL is set with its own button.
    valueParser: (p) => p.newValue ?? "",
    valueFormatter: (p) =>
      p.value === null
        ? "NULL"
        : p.value === undefined
          ? t("grid.default")
          : vectorCols.value.has(name)
            ? vectorText(p.value)
            : String(p.value) + (p.data?.cut.includes(i) ? "…" : ""),
    tooltipValueGetter: (p) => (p.data?.cut.includes(i) ? t("grid.cutHint") : undefined),
    editable: (p) => editable.value && !readOnlyCol(name) && !p.data?.deleted && !p.data?.cut.includes(i),
    // Rows come sorted from the server: a constant comparator keeps that order on the client.
    sortable: !dirty.value,
    comparator: () => 0,
    cellClassRules: {
      "text-muted-foreground italic": (p) => p.value == null,
      "text-muted-foreground": (p) => !!p.data?.cut.includes(i),
      "bg-warning/15!": (p) => !!p.data?.orig && p.data.v[i] !== p.data.orig[i] && p.data.v[i] !== undefined,
    },
  })),
);


function onReady(e: GridReadyEvent<GridRow>) {
  grid = e.api;
}

function onSort(e: SortChangedEvent) {
  order.value = e.api
    .getColumnState()
    .filter((c) => c.sort)
    .sort((a, b) => (a.sortIndex ?? 0) - (b.sortIndex ?? 0))
    .map((c) => ({ column: columns.value[Number(c.colId)], desc: c.sort === "desc" }));
  load();
}

function refreshStyles() {
  tick.value++;
  grid?.redrawRows();
}

function addRow() {
  const r: GridRow = { id: nextId++, v: columns.value.map(() => undefined), orig: null, deleted: false, cut: [] };
  rows.value = [...rows.value, r];
  tick.value++;
  setTimeout(() => grid?.ensureIndexVisible(rows.value.length - 1));
}

function deleteSelected() {
  const sel = grid?.getSelectedRows() ?? [];
  const drop = new Set(sel.filter((r) => !r.orig));
  for (const r of sel) r.deleted = !r.deleted; // toggle: a second click restores the row
  rows.value = rows.value.filter((r) => !drop.has(r));
  refreshStyles();
}

function setNull() {
  const cell = grid?.getFocusedCell();
  const node = cell && grid?.getDisplayedRowAtIndex(cell.rowIndex);
  if (cell && node?.data && editable.value && !readOnlyCol(columns.value[Number(cell.column.getColId())]) && !node.data.deleted) {
    node.setDataValue(cell.column, null);
  }
}

// JSON viewer / editor on the focused cell; editing follows the same rules as the grid.
const json = ref<InstanceType<typeof JsonDialog> | null>(null);
let jsonCell: { row: number; col: string } | null = null;
async function openJson() {
  const cell = grid?.getFocusedCell();
  const node = cell && grid?.getDisplayedRowAtIndex(cell.rowIndex);
  if (!cell || !node?.data) return;
  const i = Number(cell.column.getColId());
  jsonCell = { row: cell.rowIndex, col: cell.column.getColId() };
  const row = node.data;
  // No primary key to read a cut value by: the SQL editor gets a query finding the row by its other values.
  if (row.cut.includes(i) && !hasKey.value && row.orig) {
    emit("editSql", cellSql(row, i));
    return;
  }
  // A cut value: read it whole first (it becomes the row's original value, so not a change).
  if (row.cut.includes(i) && hasKey.value && row.orig) {
    try {
      const full = await api.cellValue(props.sessionId, props.schema, props.table, keyOf(row), columns.value[i]);
      row.orig[i] = full;
      row.v[i] = full;
      row.cut = row.cut.filter((c) => c !== i);
      refreshStyles();
    } catch (e) {
      error.value = toAppError(e);
      return;
    }
  }
  const v = row.v[i];
  json.value?.open({ column: columns.value[i], value: v ?? null, editable: editable.value && !readOnlyCol(columns.value[i]) && !row.deleted && !row.cut.includes(i) });
}
const quoteName = (n: string) => "`" + n.replace(/`/g, "``") + "`";
/** Text literal that reads the same with or without NO_BACKSLASH_ESCAPES: hex when it holds a backslash. */
function literal(v: string) {
  if (!v.includes("\\")) return "'" + v.replace(/'/g, "''") + "'";
  return "_utf8mb4 X'" + [...new TextEncoder().encode(v)].map((b) => b.toString(16).padStart(2, "0")).join("") + "'";
}
/** SELECT of one cut cell of a table without a primary key: the row is matched on its other exact values. */
function cellSql(row: GridRow, i: number) {
  const conds = columns.value.flatMap((name, k) => {
    const v = row.orig![k];
    if (k === i || row.cut.includes(k) || /float|double|real|vector|geometry/i.test(props.info.columns.find((c) => c.name === name)?.dataType ?? "")) return [];
    // Binary values that are not UTF-8 come as 0x… hex, which SQL reads back as the same bytes.
    const lit = v === null ? "NULL" : readOnlyCol(name) && /^0x[0-9A-F]*$/.test(v) ? v : literal(v);
    return [`${quoteName(name)} <=> ${lit}`];
  });
  const where = conds.length ? `\nWHERE ${conds.join("\n  AND ")}` : "";
  return `-- ${t("grid.noKeyCellSql")}\nSELECT ${quoteName(columns.value[i])}\nFROM ${quoteName(props.schema)}.${quoteName(props.table)}${where}\nLIMIT 1;`;
}

function applyJson(text: string) {
  const node = jsonCell && grid?.getDisplayedRowAtIndex(jsonCell.row);
  if (jsonCell && node) node.setDataValue(jsonCell.col, text);
}

function discard() {
  rows.value = rows.value.filter((r) => r.orig).map((r) => ({ ...r, v: [...r.orig!], deleted: false }));
  tick.value++;
}

// Save: show the generated SQL, then run it in one transaction.
const dialog = ref<HTMLDialogElement | null>(null);
const preview = ref<string[]>([]);
const saveError = ref<AppError | null>(null);

async function openPreview() {
  grid?.stopEditing();
  saveError.value = null;
  try {
    preview.value = await api.previewChanges(props.schema, props.table, changes.value);
    dialog.value?.showModal();
  } catch (e) {
    error.value = toAppError(e);
  }
}

async function apply() {
  busy.value = true;
  saveError.value = null;
  try {
    await api.applyChanges(props.sessionId, props.schema, props.table, changes.value);
    dialog.value?.close();
    await load();
  } catch (e) {
    saveError.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

const historyDialog = ref<HTMLDialogElement | null>(null);
const history = shallowRef<RowHistory | null>(null);

async function openHistory() {
  const row = grid?.getSelectedRows().find((r) => r.orig);
  if (!row) return;
  try {
    history.value = await api.rowHistory(props.sessionId, props.schema, props.table, keyOf(row));
    historyDialog.value?.showModal();
  } catch (e) {
    error.value = toAppError(e);
  }
}

/** Period values are UTC; the open end of the current version is the TIMESTAMP maximum. */
function periodValue(v: string | null) {
  if (v === null) return "NULL";
  if (/^(2038-01-19 03:14:07|2106-02-07 06:28:15)/.test(v)) return t("versioned.current");
  return new Date(v.replace(" ", "T") + "Z").toLocaleString(locale.value);
}

const historyCols = computed<ColDef<(string | null)[]>[]>(() => {
  const h = history.value;
  return (h?.rows.columns ?? []).map((name, i) => {
    const period = name === h!.start || name === h!.end;
    return {
      colId: String(i),
      headerName: name,
      valueGetter: (p) => p.data?.[i],
      valueFormatter: (p) => (period ? periodValue(p.value) : p.value === null ? "NULL" : String(p.value)),
      cellClassRules: { "text-muted-foreground italic": (p) => p.value === null, "font-semibold": () => period },
    };
  });
});

onMounted(load);
const btn = "flex h-6 shrink-0 items-center gap-1 rounded-sm px-2 whitespace-nowrap hover:bg-secondary disabled:opacity-40 disabled:hover:bg-transparent";
</script>

<template>
  <div class="flex flex-col">
    <!-- ponytail: narrow windows scroll the toolbar sideways; group actions in a menu if it gets crowded. -->
    <div class="flex h-9 shrink-0 items-center gap-1 overflow-x-auto border-b px-2">
      <form class="flex min-w-40 flex-1 items-center gap-1" @submit.prevent="load()">
        <Filter class="size-3.5 shrink-0 text-muted-foreground" />
        <input
          v-model="filter"
          :disabled="dirty"
          :placeholder="t('grid.filterPlaceholder')"
          :aria-label="t('grid.filter')"
          class="h-6 min-w-0 flex-1 rounded-sm border bg-background px-2 font-mono text-xs disabled:opacity-50"
        />
        <button type="submit" :disabled="dirty || busy" :class="btn" :title="t('grid.reload')">
          <RefreshCw class="size-3.5" /><span class="sr-only">{{ t("grid.reload") }}</span>
        </button>
      </form>
      <template v-if="versioned">
        <span class="mx-1 h-4 border-l" />
        <label class="flex shrink-0 items-center gap-1 whitespace-nowrap text-muted-foreground" :title="t('versioned.asOfHint')">
          <History class="size-3.5" />{{ t("versioned.asOf") }}
          <input
            v-model="asOfLocal"
            type="datetime-local"
            step="1"
            :disabled="dirty || busy"
            class="h-6 rounded-sm border bg-background px-1 font-mono text-xs text-foreground disabled:opacity-50"
            @change="load()"
          />
        </label>
        <button v-if="asOfLocal" :class="btn" :title="t('versioned.now')" @click="(asOfLocal = ''), load()">
          <X class="size-3.5" /><span class="sr-only">{{ t("versioned.now") }}</span>
        </button>
        <button :class="btn" :disabled="!hasKey || selected !== 1 || busy" :title="hasKey ? t('versioned.rowHistoryHint') : t('grid.readOnly')" @click="openHistory">
          {{ t("versioned.rowHistory") }}
        </button>
      </template>
      <span class="mx-1 h-4 border-l" />
      <button :class="btn" :disabled="busy" :title="t('json.openHint')" @click="openJson"><Braces class="size-3.5" />{{ t("json.open") }}</button>
      <template v-if="editable">
        <span class="mx-1 h-4 border-l" />
        <button :class="btn" :disabled="busy" @click="addRow"><Plus class="size-3.5" />{{ t("grid.addRow") }}</button>
        <button :class="btn" :disabled="busy" @click="deleteSelected"><Trash2 class="size-3.5" />{{ t("grid.deleteRows") }}</button>
        <button :class="btn" :disabled="busy" @click="setNull"><Ban class="size-3.5" />{{ t("grid.setNull") }}</button>
        <span class="mx-1 h-4 border-l" />
        <button :class="btn" :disabled="!dirty || busy" @click="discard"><Undo2 class="size-3.5" />{{ t("grid.discard") }}</button>
        <button
          :disabled="!dirty || busy"
          class="flex h-6 shrink-0 items-center gap-1 rounded-sm bg-primary px-2 font-semibold whitespace-nowrap text-primary-foreground disabled:opacity-40"
          @click="openPreview"
        >
          <Save class="size-3.5" />{{ t("grid.save", { n: changes.length }) }}
        </button>
      </template>
      <span v-else class="ml-1 shrink-0 whitespace-nowrap text-muted-foreground">{{ asOf ? t("versioned.pastReadOnly") : t("grid.readOnly") }}</span>
    </div>

    <ErrorLine v-if="error" :error="error" class="m-2" />

    <AgGridVue
      class="min-h-0 flex-1"
      :theme="theme"
      :row-data="rows"
      :column-defs="colDefs"
      :get-row-id="(p) => String(p.data.id)"
      :row-selection="{ mode: 'multiRow', checkboxes: false, headerCheckbox: false, enableClickSelection: true }"
      :row-class-rules="{ 'line-through opacity-50': (p) => !!p.data?.deleted, 'bg-success/10!': (p) => !!p.data && !p.data.orig }"
      :default-col-def="{ resizable: true, minWidth: 60, maxWidth: 480 }"
      :locale-text="{ noRowsToShow: t('grid.noRows'), loadingOoo: t('grid.loading') }"
      :stop-editing-when-cells-lose-focus="true"
      :loading="busy"
      @grid-ready="onReady"
      @cell-value-changed="refreshStyles"
      @sort-changed="onSort"
      @selection-changed="selected = grid?.getSelectedRows().length ?? 0"
    />

    <div class="flex h-7 shrink-0 items-center gap-2 border-t px-2 text-muted-foreground">
      <span>{{ t("grid.rowCount", rows.filter((r) => r.orig).length) }}</span>
      <button v-if="hasMore" :class="btn" :disabled="busy" class="text-foreground" @click="load(true)">{{ t("grid.loadMore", { n: PAGE }) }}</button>
    </div>

    <JsonDialog ref="json" :schema="schema" :table="table" @apply="applyJson" @sql="(sql) => emit('editSql', sql)" />

    <dialog ref="historyDialog" class="m-auto w-[min(72rem,92vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <div class="border-b px-4 py-2 font-semibold">{{ t("versioned.historyTitle", history?.rows.rows.length ?? 0) }}</div>
      <AgGridVue
        v-if="history"
        class="h-[50vh]"
        :theme="theme"
        :row-data="history.rows.rows"
        :column-defs="historyCols"
        :default-col-def="{ resizable: true, minWidth: 60, maxWidth: 480 }"
        :enable-cell-text-selection="true"
      />
      <div class="flex justify-end px-4 py-3">
        <button class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="historyDialog?.close()">{{ t("object.close") }}</button>
      </div>
    </dialog>

    <dialog ref="dialog" class="m-auto w-[min(56rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <div class="border-b px-4 py-2 font-semibold">{{ t("grid.previewTitle") }}</div>
      <pre class="max-h-[50vh] overflow-auto p-4 font-mono text-xs whitespace-pre-wrap select-text">{{ preview.join("\n") }}</pre>
      <p class="px-4 text-muted-foreground">{{ t("grid.previewHint") }}</p>
      <ErrorLine v-if="saveError" :error="saveError" class="mx-4 mt-2" />
      <div class="flex justify-end gap-2 px-4 py-3">
        <button class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="dialog?.close()">{{ t("grid.cancel") }}</button>
        <button :disabled="busy" class="h-7 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50" @click="apply">
          {{ t("grid.apply") }}
        </button>
      </div>
    </dialog>
  </div>
</template>
