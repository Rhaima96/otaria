<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, shallowRef } from "vue";
import { useI18n } from "vue-i18n";
import { AgGridVue } from "ag-grid-vue3";
import { SplitterGroup, SplitterPanel, SplitterResizeHandle } from "reka-ui";
import type { ColDef, GridApi, GridReadyEvent } from "ag-grid-community";
import { Braces, Gauge, History, Play, Square, Star, TextCursorInput, Trash2, X } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import JsonDialog from "@/components/JsonDialog.vue";
import PlanNode from "@/components/PlanNode.vue";
import { useQueryLibrary } from "@/composables/useQueryLibrary";
import { api, toAppError, type AppError, type RunOutput, type RunRequest } from "@/lib/api";
import { theme, vectorText } from "@/lib/grid";
import { parsePlan } from "@/lib/plan";
import { monaco, sources } from "@/lib/monaco";
import { cn } from "@/lib/utils";

/** `initialSql`: script to start with (object edit, template). `mariadb`: offer ANALYZE FORMAT=JSON (hidden on MySQL). */
const props = defineProps<{ sessionId: string; profileId: string; editorId: string; schema: string | null; initialSql?: string; mariadb: boolean }>();
const { t, locale } = useI18n();
const lib = useQueryLibrary(props.profileId);

const host = ref<HTMLElement | null>(null);
let editor: monaco.editor.IStandaloneCodeEditor | null = null;

const schema = ref(props.schema);
const running = ref(false);
const output = shallowRef<RunOutput | null>(null);
const error = ref<AppError | null>(null);
/** Index in output.results, or "messages". */
const resultTab = ref<number | "messages">("messages");
const panel = ref<"history" | "favorites" | null>(null);

const MAX_ROWS_KEY = "otaria.maxRows";
const maxRows = ref(Number(localStorage.getItem(MAX_ROWS_KEY)) || 1000);

// Completion metadata: loaded on demand, cached for this tab only.
const memo = new Map<string, Promise<string[]>>();
const once = (key: string, load: () => Promise<string[]>) => {
  if (!memo.has(key)) memo.set(key, load().catch((e) => (memo.delete(key), Promise.reject(e))));
  return memo.get(key)!;
};
const completion = {
  schema: () => schema.value,
  schemas: () => once("", () => api.listSchemas(props.sessionId)),
  tables: (s: string) =>
    once(`t:${s}`, async () => (await api.listObjects(props.sessionId, s)).filter((o) => ["table", "view", "sequence"].includes(o.kind)).map((o) => o.name)),
  columns: (s: string, tb: string) => once(`c:${s}.${tb}`, async () => (await api.tableInfo(props.sessionId, s, tb)).columns.map((c) => c.name)),
};

onMounted(() => {
  editor = monaco.editor.create(host.value!, {
    value: props.initialSql ?? "",
    language: "mysql",
    theme: "otaria",
    automaticLayout: true,
    minimap: { enabled: false },
    fontFamily: getComputedStyle(document.documentElement).getPropertyValue("--font-mono"),
    fontSize: 13,
    scrollBeyondLastLine: false,
    tabSize: 2,
  });
  sources.set(editor.getModel()!, completion);
  const { KeyMod, KeyCode } = monaco;
  editor.addCommand(KeyMod.CtrlCmd | KeyCode.Enter, () => run(false));
  editor.addCommand(KeyMod.CtrlCmd | KeyMod.Shift | KeyCode.Enter, () => run(true));
  editor.addCommand(KeyCode.F9, () => run(false));
  editor.onDidChangeModelContent(() => (text.value = editor!.getValue()));
  editor.focus();
});

// Unsaved text: changed since the tab opened or since the whole script last ran (that is in the
// history). SessionView asks before closing the tab.
const text = ref(props.initialSql ?? "");
const ran = ref(props.initialSql ?? "");
defineExpose({ dirty: computed(() => text.value.trim() !== ran.value.trim()) });

onBeforeUnmount(() => {
  editor?.getModel()?.dispose();
  editor?.dispose();
  api.closeEditor(props.sessionId, props.editorId).catch(() => {});
});

// Destructive statements: the backend refuses them until confirmed here.
const confirmDialog = ref<HTMLDialogElement | null>(null);
const pending = ref<{ req: RunRequest; statements: string } | null>(null);

/** `analyze`: the selection or the statement under the cursor, as ANALYZE FORMAT=JSON. */
async function run(currentOnly: boolean, analyze = false) {
  if (!editor || running.value) return;
  const model = editor.getModel()!;
  const sel = editor.getSelection();
  const selected = sel && !sel.isEmpty() ? model.getValueInRange(sel) : "";
  const sql = selected || model.getValue();
  if (!currentOnly && !analyze && !selected) ran.value = sql;
  if (!sql.trim()) return;
  const offset = model.getOffsetAt(editor.getPosition()!);
  const cursor = (currentOnly || analyze) && !selected ? new TextEncoder().encode(sql.slice(0, offset)).length : null;
  await send({ schema: schema.value, sql, cursor, maxRows: maxRows.value, confirmed: false, analyze });
}

async function send(req: RunRequest) {
  running.value = true;
  error.value = null;
  localStorage.setItem(MAX_ROWS_KEY, String(req.maxRows));
  try {
    const out = await api.runSql(props.sessionId, props.editorId, req);
    output.value = out;
    counts.value = {};
    if (out.schema !== schema.value) memo.clear();
    schema.value = out.schema;
    const firstSet = out.results.findIndex((r) => r.columns.length);
    resultTab.value = out.error || firstSet < 0 ? "messages" : firstSet;
    lib.remember(req.sql, req.schema);
  } catch (e) {
    const err = toAppError(e);
    if (err.code === "CONFIRM_RISKY") {
      pending.value = { req: { ...req, confirmed: true }, statements: err.detail ?? "" };
      confirmDialog.value?.showModal();
    } else error.value = err;
  } finally {
    running.value = false;
  }
}

/** Exact row counts of capped reads, by result tab, asked for with the Count button. */
const counts = ref<Record<number, number>>({});
async function countRows() {
  const i = resultTab.value;
  const r = shown.value;
  if (typeof i !== "number" || !r || running.value) return;
  running.value = true;
  error.value = null;
  try {
    counts.value = { ...counts.value, [i]: await api.countSqlRows(props.sessionId, props.editorId, schema.value, r.sql) };
  } catch (e) {
    error.value = toAppError(e);
  } finally {
    running.value = false;
  }
}

function confirmRun() {
  confirmDialog.value?.close();
  if (pending.value) send(pending.value.req);
  pending.value = null;
}

async function cancel() {
  try {
    await api.cancelSql(props.sessionId, props.editorId);
  } catch (e) {
    error.value = toAppError(e);
  }
}

function insert(sql: string) {
  editor?.setValue(sql);
  editor?.focus();
}

// Favorites: name prompt.
const favDialog = ref<HTMLDialogElement | null>(null);
const favName = ref("");
function openFavorite() {
  favName.value = "";
  favDialog.value?.showModal();
}
function saveFavorite() {
  const sql = editor?.getValue().trim();
  if (favName.value.trim() && sql) lib.saveFavorite(favName.value.trim(), sql);
  favDialog.value?.close();
  panel.value = "favorites";
}

const sets = computed(() => (output.value?.results ?? []).map((r, i) => ({ r, i })).filter((x) => x.r.columns.length));
const shown = computed(() => (typeof resultTab.value === "number" ? output.value?.results[resultTab.value] : undefined));
const colDefs = computed<ColDef<(string | null)[]>[]>(() =>
  (shown.value?.columns ?? []).map((name, i) => ({
    colId: String(i),
    headerName: name,
    valueGetter: (p) => p.data?.[i],
    valueFormatter: (p) => (p.value === null ? "NULL" : shown.value?.vectorColumns.includes(i) ? vectorText(p.value) : String(p.value)),
    cellClassRules: { "text-muted-foreground italic": (p) => p.value === null },
  })),
);

// JSON viewer on the focused result cell (read-only). Its JSON_TABLE helper needs the column's
// table: offered only when the column comes from one; the query lands at the end of this editor.
let gridApi: GridApi | null = null;
const onGridReady = (e: GridReadyEvent) => (gridApi = e.api);
const json = ref<InstanceType<typeof JsonDialog> | null>(null);
const jsonOrigin = ref<{ schema: string; table: string } | null>(null);
function openJson() {
  const cell = gridApi?.getFocusedCell();
  const r = shown.value;
  if (!cell || !r) return;
  const i = Number(cell.column.getColId());
  const o = r.origins[i];
  jsonOrigin.value = o && { schema: o.schema, table: o.table };
  json.value?.open({ column: o?.column ?? r.columns[i], value: r.rows[cell.rowIndex]?.[i] ?? null, editable: false });
}
function appendSql(sql: string) {
  if (!editor) return;
  const end = editor.getModel()!.getFullModelRange().getEndPosition();
  editor.executeEdits("json-table", [{ range: new monaco.Range(end.lineNumber, end.column, end.lineNumber, end.column), text: `\n\n${sql}` }]);
  editor.revealLine(editor.getModel()!.getLineCount());
  editor.focus();
}

/** A one-cell EXPLAIN / ANALYZE FORMAT=JSON result is drawn as a tree (toggle back to the grid). */
const plan = computed(() => (shown.value?.columns.length === 1 && shown.value.rows.length === 1 ? parsePlan(shown.value.rows[0][0]) : null));
const planView = ref(true);

const fmt = (iso: string) => new Date(iso).toLocaleString(locale.value);
const btn = "flex h-6 items-center gap-1 rounded-sm px-2 hover:bg-secondary disabled:opacity-40 disabled:hover:bg-transparent";
const tabBtn = "flex h-full shrink-0 items-center gap-1 border-r px-3 hover:bg-secondary";
</script>

<template>
  <div class="flex flex-col">
    <div class="flex h-9 shrink-0 items-center gap-1 border-b px-2">
      <button
        :disabled="running"
        class="flex h-6 items-center gap-1 rounded-sm bg-primary px-2 font-semibold text-primary-foreground disabled:opacity-40"
        :title="t('editor.runHint')"
        @click="run(false)"
      >
        <Play class="size-3.5" />{{ t("editor.run") }}
      </button>
      <button :class="btn" :disabled="running" :title="t('editor.runCurrentHint')" @click="run(true)">
        <TextCursorInput class="size-3.5" />{{ t("editor.runCurrent") }}
      </button>
      <button v-if="mariadb" :class="btn" :disabled="running" :title="t('editor.analyzeHint')" @click="run(true, true)">
        <Gauge class="size-3.5" />{{ t("editor.analyze") }}
      </button>
      <button :class="btn" :disabled="!running" @click="cancel"><Square class="size-3.5" />{{ t("editor.cancel") }}</button>
      <span class="mx-1 h-4 border-l" />
      <label class="flex items-center gap-1 text-muted-foreground">
        {{ t("editor.maxRows") }}
        <input v-model.number="maxRows" type="number" min="1" max="1000000" class="h-6 w-20 rounded-sm border bg-background px-1 font-mono text-xs text-foreground" />
      </label>
      <span class="ml-auto truncate font-mono text-muted-foreground" :title="t('editor.schema')">{{ schema ?? t("editor.noSchema") }}</span>
      <span class="mx-1 h-4 border-l" />
      <button :class="cn(btn, panel === 'history' && 'bg-secondary')" @click="panel = panel === 'history' ? null : 'history'">
        <History class="size-3.5" />{{ t("editor.history") }}
      </button>
      <button :class="cn(btn, panel === 'favorites' && 'bg-secondary')" @click="panel = panel === 'favorites' ? null : 'favorites'">
        <Star class="size-3.5" />{{ t("editor.favorites") }}
      </button>
    </div>

    <div class="flex min-h-0 flex-1">
      <div class="flex min-w-0 flex-1 flex-col">
        <SplitterGroup direction="vertical" auto-save-id="otaria.editorSplit">
          <SplitterPanel :default-size="45" :min-size="10">
            <div ref="host" class="h-full" />
          </SplitterPanel>
          <SplitterResizeHandle class="h-1 shrink-0 border-b hover:bg-primary data-[state=drag]:bg-primary" />
          <SplitterPanel :default-size="55" :min-size="10" class="flex flex-col">
        <ErrorLine v-if="error" :error="error" class="m-2" />

        <template v-if="output">
          <nav class="flex h-8 shrink-0 items-stretch overflow-x-auto border-b bg-sidebar" role="tablist">
            <button v-for="x in sets" :key="x.i" role="tab" :aria-selected="resultTab === x.i" :class="cn(tabBtn, resultTab === x.i && 'bg-background')" @click="resultTab = x.i">
              {{ t("editor.result", { n: x.i + 1 }) }}
            </button>
            <button role="tab" :aria-selected="resultTab === 'messages'" :class="cn(tabBtn, resultTab === 'messages' && 'bg-background', output.error && 'text-destructive')" @click="resultTab = 'messages'">
              {{ t("editor.messages") }}
            </button>
          </nav>

          <template v-if="shown">
            <div v-if="plan && planView" class="min-h-0 flex-1 overflow-auto p-3 pl-6">
              <PlanNode :node="plan.root" :total="plan.total" />
            </div>
            <AgGridVue
              v-else
              class="min-h-0 flex-1"
              :theme="theme"
              :row-data="shown.rows"
              :column-defs="colDefs"
              :default-col-def="{ resizable: true, sortable: true, minWidth: 60, maxWidth: 480 }"
              :locale-text="{ noRowsToShow: t('grid.noRows') }"
              :enable-cell-text-selection="true"
              @grid-ready="onGridReady"
            />
            <div class="flex h-7 shrink-0 items-center gap-2 border-t px-2 text-muted-foreground">
              <span>{{ t("editor.rows", shown.rows.length) }}</span>
              <template v-if="shown.stopped && typeof resultTab === 'number'">
                <span v-if="counts[resultTab] !== undefined" class="text-warning">{{ t("editor.cappedTotal", { total: counts[resultTab] }) }}</span>
                <template v-else>
                  <span class="text-warning">{{ t("editor.cappedStopped") }}</span>
                  <button :class="btn" class="text-foreground" :disabled="running" :title="t('editor.countHint')" @click="countRows">{{ t("editor.count") }}</button>
                </template>
              </template>
              <span v-else-if="shown.totalRows > shown.rows.length" class="text-warning">{{ t("editor.capped", { total: shown.totalRows }) }}</span>
              <button v-if="!(plan && planView)" :class="btn" class="text-foreground" :title="t('json.openHint')" @click="openJson"><Braces class="size-3.5" />{{ t("json.open") }}</button>
              <button v-if="plan" :class="btn" class="text-foreground" @click="planView = !planView">{{ planView ? t("plan.showGrid") : t("plan.showTree") }}</button>
              <span class="ml-auto">{{ t("editor.elapsed", { ms: shown.elapsedMs }) }}</span>
            </div>
          </template>

          <ul v-else class="min-h-0 flex-1 overflow-auto p-2 font-mono text-xs">
            <li v-for="(r, i) in output.results" :key="i" class="border-b py-1.5">
              <div class="truncate text-muted-foreground">{{ r.sql }}</div>
              <div>
                {{ r.columns.length ? t(r.stopped ? "editor.rowsAtLeast" : "editor.rows", r.totalRows) : t("editor.affected", r.affectedRows) }}
                · {{ t("editor.elapsed", { ms: r.elapsedMs }) }}
                <span v-if="r.warnings" class="text-warning">· {{ t("editor.warnings", r.warnings) }}</span>
              </div>
            </li>
            <li v-if="output.error" class="py-1.5">
              <div class="truncate text-muted-foreground">{{ output.errorSql }}</div>
              <ErrorLine :error="output.error" class="mt-1" />
            </li>
            <li v-if="!output.results.length && !output.error" class="py-1.5 text-muted-foreground">{{ t("editor.nothing") }}</li>
          </ul>
        </template>
        <p v-else class="p-3 text-muted-foreground">{{ t("editor.hint") }}</p>
          </SplitterPanel>
        </SplitterGroup>
      </div>

      <aside v-if="panel" class="flex w-72 shrink-0 flex-col border-l bg-sidebar">
        <div class="flex h-8 shrink-0 items-center gap-1 border-b px-2">
          <span class="font-medium text-muted-foreground">{{ t(`editor.${panel}`) }}</span>
          <button v-if="panel === 'favorites'" :class="cn(btn, 'ml-auto')" @click="openFavorite"><Star class="size-3.5" />{{ t("editor.addFavorite") }}</button>
          <button v-else :class="cn(btn, 'ml-auto')" :disabled="!lib.history.value.length" @click="lib.history.value = []">
            <Trash2 class="size-3.5" />{{ t("editor.clearHistory") }}
          </button>
        </div>
        <ul class="min-h-0 flex-1 overflow-auto">
          <template v-if="panel === 'history'">
            <li v-for="(h, i) in lib.history.value" :key="i">
              <button class="w-full border-b px-2 py-1.5 text-left hover:bg-secondary" :title="h.sql" @click="insert(h.sql)">
                <div class="line-clamp-2 font-mono text-xs break-all">{{ h.sql }}</div>
                <div class="text-muted-foreground">{{ fmt(h.at) }}<span v-if="h.schema"> · {{ h.schema }}</span></div>
              </button>
            </li>
            <li v-if="!lib.history.value.length" class="p-2 text-muted-foreground">{{ t("editor.emptyHistory") }}</li>
          </template>
          <template v-else>
            <li v-for="f in lib.favorites.value" :key="f.name" class="flex items-start border-b hover:bg-secondary">
              <button class="min-w-0 flex-1 px-2 py-1.5 text-left" :title="f.sql" @click="insert(f.sql)">
                <div class="truncate font-medium">{{ f.name }}</div>
                <div class="line-clamp-2 font-mono text-xs break-all text-muted-foreground">{{ f.sql }}</div>
              </button>
              <button :title="t('editor.removeFavorite')" class="m-1 flex size-5 items-center justify-center rounded-sm text-muted-foreground hover:text-foreground" @click="lib.removeFavorite(f.name)">
                <X class="size-3" /><span class="sr-only">{{ t("editor.removeFavorite") }}</span>
              </button>
            </li>
            <li v-if="!lib.favorites.value.length" class="p-2 text-muted-foreground">{{ t("editor.emptyFavorites") }}</li>
          </template>
        </ul>
      </aside>
    </div>

    <dialog ref="confirmDialog" class="m-auto w-[min(48rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <div class="border-b px-4 py-2 font-semibold text-destructive">{{ t("editor.confirmTitle") }}</div>
      <p class="px-4 pt-3">{{ t("editor.confirmText") }}</p>
      <pre class="max-h-[40vh] overflow-auto p-4 font-mono text-xs whitespace-pre-wrap select-text">{{ pending?.statements }}</pre>
      <div class="flex justify-end gap-2 px-4 py-3">
        <button class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="confirmDialog?.close()">{{ t("grid.cancel") }}</button>
        <button class="h-7 rounded-sm bg-destructive px-3 font-semibold text-primary-foreground" @click="confirmRun">{{ t("editor.confirmRun") }}</button>
      </div>
    </dialog>

    <dialog ref="favDialog" class="m-auto w-[min(28rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <form class="flex flex-col gap-3 p-4" @submit.prevent="saveFavorite">
        <label class="flex flex-col gap-1">
          <span class="font-semibold">{{ t("editor.favoriteName") }}</span>
          <input v-model="favName" required autofocus class="h-7 rounded-sm border bg-background px-2" />
        </label>
        <div class="flex justify-end gap-2">
          <button type="button" class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="favDialog?.close()">{{ t("grid.cancel") }}</button>
          <button type="submit" class="h-7 rounded-sm bg-primary px-3 font-semibold text-primary-foreground">{{ t("editor.save") }}</button>
        </div>
      </form>
    </dialog>
    <JsonDialog ref="json" :schema="jsonOrigin?.schema ?? ''" :table="jsonOrigin?.table ?? ''" @sql="appendSql" />
  </div>
</template>
