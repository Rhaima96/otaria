<script setup lang="ts">
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import { listen } from "@tauri-apps/api/event";
import { open as openFile, save } from "@tauri-apps/plugin-dialog";
import { Copy, Download, FileSpreadsheet, LoaderCircle, Upload } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { useSessions } from "@/composables/useSessions";
import { api, toAppError, type AppError, type CsvPreview, type ExportFormat, type TransferProgress } from "@/lib/api";

/**
 * One dialog for every transfer: export a table / view / schema (to a file or another connection),
 * import an SQL file into a schema, or a CSV file into a table.
 */
export interface TransferTarget {
  mode: "export" | "import" | "csv";
  schema: string;
  /** One table or view; null = whole schema (export / import only). */
  table: string | null;
  /** Views export their rows to files only. */
  view?: boolean;
}

const props = defineProps<{ sessionId: string }>();
const emit = defineEmits<{ imported: [schema: string, mode: TransferTarget["mode"]] }>();
const { t } = useI18n();
const { sessions } = useSessions();

const dialog = ref<HTMLDialogElement | null>(null);
const target = ref<TransferTarget>({ mode: "export", schema: "", table: null });
const dest = ref<"file" | "connection">("file");
const format = ref<ExportFormat>("csv");
const data = ref(true);
const drop = ref(false);
const running = ref(false);
const jobId = ref("");
const progress = ref<TransferProgress | null>(null);
const result = ref("");
const error = ref<AppError | null>(null);

const formats = computed<ExportFormat[]>(() =>
  target.value.table === null ? ["sql"] : target.value.view ? ["csv", "json", "xlsx"] : ["csv", "json", "xlsx", "sql"],
);
const name = computed(() => target.value.table ?? target.value.schema);
/** SQL options apply to an SQL file and to a copy (which is an SQL export + import). */
const sqlOptions = computed(() => target.value.mode === "export" && (dest.value === "connection" || format.value === "sql"));

// Copy to another connection (or another schema of this one).
const targetSession = ref("");
const targetSchema = ref("");
const targetSchemas = ref<string[]>([]);
async function pickSession() {
  targetSchemas.value = [];
  targetSchema.value = targetSession.value === props.sessionId ? `${target.value.schema}_copy` : target.value.schema;
  targetSchemas.value = await api.listSchemas(targetSession.value).catch(() => []);
}

// CSV import.
const csvPath = ref("");
const delimiter = ref<string | null>(null);
const header = ref(true);
const emptyNull = ref(true);
const preview = ref<CsvPreview | null>(null);
const tableColumns = ref<string[]>([]);
const mapping = ref<(string | null)[]>([]);
const DELIMITERS = { ",": "comma", ";": "semicolon", "\t": "tab", "|": "pipe" } as const;

function open(x: TransferTarget) {
  target.value = x;
  dest.value = "file";
  format.value = formats.value[0];
  data.value = true;
  drop.value = false;
  progress.value = null;
  result.value = "";
  error.value = null;
  targetSession.value = "";
  csvPath.value = "";
  preview.value = null;
  tableColumns.value = [];
  delimiter.value = null;
  header.value = true;
  emptyNull.value = true;
  dialog.value?.showModal();
}
defineExpose({ open });

const sizeFmt = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });
const mb = (bytes: number) => sizeFmt.format(bytes / 1048576);

/** Runs `job` with progress events for its id; the dialog cannot be closed meanwhile. */
async function track(job: (id: string) => Promise<number>, done: (n: number) => string) {
  const id = crypto.randomUUID();
  jobId.value = id;
  running.value = true;
  progress.value = null;
  result.value = "";
  error.value = null;
  const stop = await listen<TransferProgress>("transfer-progress", (e) => {
    if (e.payload.id === id) progress.value = e.payload;
  });
  try {
    result.value = done(await job(id));
    return true;
  } catch (e) {
    error.value = toAppError(e);
    return false;
  } finally {
    stop();
    running.value = false;
  }
}

async function runExport() {
  const { schema, table } = target.value;
  if (dest.value === "connection") {
    const to = sessions.value.find((s) => s.sessionId === targetSession.value);
    const req = { schema, table, targetSession: targetSession.value, targetSchema: targetSchema.value.trim(), data: data.value, drop: drop.value };
    await track(
      (id) => api.copyData(props.sessionId, id, req),
      (n) => t("transfer.copied", { n, to: `${to?.profile.name} · ${req.targetSchema}` }),
    );
    emit("imported", req.targetSchema, "export");
    return;
  }
  const ext = format.value;
  const path = await save({ defaultPath: `${name.value}.${ext}`, filters: [{ name: ext.toUpperCase(), extensions: [ext] }] });
  if (!path) return;
  await track(
    (id) => api.exportData(props.sessionId, id, { schema, table, format: format.value, path, data: data.value, drop: drop.value }),
    (n) => t("transfer.exported", { n, path }),
  );
}

async function runImport() {
  const path = await openFile({ multiple: false, directory: false, filters: [{ name: "SQL", extensions: ["sql"] }] });
  if (typeof path !== "string") return;
  const schema = target.value.schema;
  await track(
    (id) => api.importSql(props.sessionId, id, schema, path),
    (n) => t("transfer.imported", { n }),
  );
  // Even a stopped import may have created objects.
  emit("imported", schema, target.value.mode);
}

async function pickCsv() {
  const path = await openFile({ multiple: false, directory: false, filters: [{ name: "CSV", extensions: ["csv", "tsv", "txt"] }] });
  if (typeof path !== "string") return;
  csvPath.value = path;
  delimiter.value = null;
  await loadPreview();
}

/** Reads the first rows again (delimiter or header changed) and guesses the column mapping. */
async function loadPreview() {
  error.value = null;
  result.value = "";
  try {
    const { schema, table } = target.value;
    if (!tableColumns.value.length) {
      const info = await api.tableInfo(props.sessionId, schema, table!);
      // Generated columns cannot receive values.
      tableColumns.value = info.columns.filter((c) => !/GENERATED/i.test(c.extra)).map((c) => c.name);
    }
    preview.value = await api.csvPreview(csvPath.value, delimiter.value, header.value);
    delimiter.value = preview.value.delimiter;
    const byName = (n: string) => tableColumns.value.find((c) => c.toLowerCase() === n.trim().toLowerCase()) ?? null;
    mapping.value = preview.value.columns.map((c, i) => (header.value ? byName(c) : (tableColumns.value[i] ?? null)));
  } catch (e) {
    preview.value = null;
    error.value = toAppError(e);
  }
}

async function runCsv() {
  const { schema, table } = target.value;
  const req = { schema, table: table!, path: csvPath.value, delimiter: delimiter.value!, header: header.value, mapping: mapping.value, emptyNull: emptyNull.value };
  if (await track((id) => api.importCsv(props.sessionId, id, req), (n) => t("transfer.csvImported", { n }))) emit("imported", schema, target.value.mode);
}

const canRun = computed(() => {
  if (running.value) return false;
  if (target.value.mode === "csv") return !!preview.value && mapping.value.some((m) => m);
  if (target.value.mode === "export" && dest.value === "connection") return !!targetSession.value && !!targetSchema.value.trim();
  return true;
});

function run() {
  if (target.value.mode === "export") runExport();
  else if (target.value.mode === "import") runImport();
  else runCsv();
}

const runLabel = computed(() =>
  target.value.mode === "csv"
    ? t("transfer.csvRun")
    : target.value.mode === "import"
      ? t("transfer.chooseOpen")
      : dest.value === "connection"
        ? t("transfer.copyRun")
        : t("transfer.chooseSave"),
);
const runIcon = computed(() => (target.value.mode === "export" ? (dest.value === "connection" ? Copy : Download) : Upload));
const title = computed(() =>
  target.value.mode === "export" ? t("transfer.exportTitle", { name: name.value }) : t("transfer.importTitle", { name: name.value }),
);

const btn = "flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary disabled:opacity-50";
const field = "h-7 rounded-sm border bg-background px-2";
const cell = "border-b px-2 py-1 text-left align-top";
</script>

<template>
  <dialog
    ref="dialog"
    class="m-auto w-[min(48rem,92vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40"
    @cancel="(e) => running && e.preventDefault()"
  >
    <div class="border-b px-4 py-2 font-semibold">{{ title }}</div>

    <div class="max-h-[70vh] space-y-3 overflow-auto p-4">
      <template v-if="target.mode === 'export'">
        <fieldset v-if="!target.view" :disabled="running" class="flex flex-wrap gap-3">
          <legend class="mb-1 text-muted-foreground">{{ t("transfer.destination") }}</legend>
          <label class="flex items-center gap-1.5"><input v-model="dest" type="radio" value="file" class="size-3.5" />{{ t("transfer.toFile") }}</label>
          <label class="flex items-center gap-1.5"><input v-model="dest" type="radio" value="connection" class="size-3.5" />{{ t("transfer.toConnection") }}</label>
        </fieldset>
        <fieldset v-if="dest === 'file' && formats.length > 1" :disabled="running" class="flex flex-wrap gap-3">
          <legend class="mb-1 text-muted-foreground">{{ t("transfer.format") }}</legend>
          <label v-for="f in formats" :key="f" class="flex items-center gap-1.5">
            <input v-model="format" type="radio" :value="f" class="size-3.5" />{{ t(`transfer.formats.${f}`) }}
          </label>
        </fieldset>
        <div v-if="dest === 'connection'" class="grid grid-cols-[10rem_1fr] items-center gap-2">
          <label for="tr-session" class="text-muted-foreground">{{ t("transfer.targetConnection") }}</label>
          <select id="tr-session" v-model="targetSession" :disabled="running" :class="field" @change="pickSession">
            <option v-for="s in sessions" :key="s.sessionId" :value="s.sessionId">{{ s.profile.name }}</option>
          </select>
          <label for="tr-schema" class="text-muted-foreground">{{ t("transfer.targetSchema") }}</label>
          <input id="tr-schema" v-model="targetSchema" :disabled="running || !targetSession" list="tr-schemas" :class="field" />
          <datalist id="tr-schemas"><option v-for="s in targetSchemas" :key="s" :value="s" /></datalist>
          <p class="col-start-2 text-muted-foreground">{{ t("transfer.copyHint") }}</p>
        </div>
        <div v-if="sqlOptions" class="flex flex-col gap-1.5">
          <label class="flex items-center gap-1.5"><input v-model="data" :disabled="running" type="checkbox" class="size-3.5" />{{ t("transfer.withData") }}</label>
          <label class="flex items-center gap-1.5"><input v-model="drop" :disabled="running" type="checkbox" class="size-3.5" />{{ t("transfer.withDrop") }}</label>
          <p v-if="dest === 'file'" class="text-muted-foreground">{{ t("transfer.sqlHint") }}</p>
        </div>
      </template>

      <p v-else-if="target.mode === 'import'" class="text-muted-foreground">{{ t("transfer.importHint", { schema: target.schema }) }}</p>

      <template v-else>
        <div class="flex flex-wrap items-center gap-2">
          <button :disabled="running" :class="btn" @click="pickCsv"><FileSpreadsheet class="size-3.5" />{{ t("transfer.csvPick") }}</button>
          <span class="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground" :title="csvPath">{{ csvPath }}</span>
        </div>
        <template v-if="csvPath">
          <div class="flex flex-wrap items-center gap-4">
            <label class="flex items-center gap-1.5">
              {{ t("transfer.delimiter") }}
              <select v-model="delimiter" :disabled="running" :class="field" @change="loadPreview">
                <option v-for="(key, d) in DELIMITERS" :key="d" :value="d">{{ t(`transfer.delimiters.${key}`) }}</option>
              </select>
            </label>
            <label class="flex items-center gap-1.5"><input v-model="header" :disabled="running" type="checkbox" class="size-3.5" @change="loadPreview" />{{ t("transfer.header") }}</label>
            <label class="flex items-center gap-1.5"><input v-model="emptyNull" :disabled="running" type="checkbox" class="size-3.5" />{{ t("transfer.emptyNull") }}</label>
          </div>
          <table v-if="preview" class="w-full border-collapse">
            <thead class="text-muted-foreground">
              <tr>
                <th :class="cell">{{ t("transfer.csvColumn") }}</th>
                <th :class="cell">{{ t("transfer.sample") }}</th>
                <th :class="cell">{{ t("transfer.tableColumn") }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="(c, i) in preview.columns" :key="i">
                <td :class="cell">{{ c }}</td>
                <td :class="[cell, 'max-w-60 truncate font-mono text-xs']">{{ preview.rows[0]?.[i] }}</td>
                <td :class="cell">
                  <select v-model="mapping[i]" :disabled="running" :aria-label="t('transfer.tableColumn')" :class="[field, 'w-full']">
                    <option :value="null">{{ t("transfer.skip") }}</option>
                    <option v-for="col in tableColumns" :key="col" :value="col">{{ col }}</option>
                  </select>
                </td>
              </tr>
            </tbody>
          </table>
          <p class="text-muted-foreground">{{ t("transfer.csvHint") }}</p>
        </template>
      </template>

      <div v-if="running" class="space-y-1">
        <p class="flex items-center gap-1.5 text-muted-foreground">
          <LoaderCircle class="size-3.5 animate-spin" />
          <template v-if="progress && progress.total">{{ t("transfer.progressBytes", { done: mb(progress.done), total: mb(progress.total) }) }}</template>
          <template v-else-if="progress">{{ t("transfer.progressRows", { n: progress.done }) }}</template>
          <template v-else>{{ t("transfer.starting") }}</template>
        </p>
        <progress v-if="progress && progress.total" :value="progress.done" :max="progress.total" class="h-1.5 w-full accent-primary" />
      </div>
      <p v-if="result" role="status" class="break-all text-success">{{ result }}</p>
      <template v-if="error">
        <ErrorLine :error="error" />
        <p v-if="error.detail && ['IMPORT_ROWS', 'CSV_PARSE', 'FILE_NOT_UTF8'].includes(error.code)" class="text-muted-foreground">
          {{ t("transfer.fileLines", { lines: error.detail }) }}
        </p>
        <pre v-else-if="error.detail" class="max-h-40 overflow-auto rounded-sm border bg-card p-2 font-mono text-xs whitespace-pre-wrap select-text">{{ error.detail }}</pre>
      </template>
    </div>

    <div class="flex justify-end gap-2 border-t px-4 py-3">
      <button v-if="running" :class="btn" @click="api.cancelJob(sessionId, jobId)">{{ t("transfer.stop") }}</button>
      <button v-else :class="btn" @click="dialog?.close()">{{ t("transfer.close") }}</button>
      <button :disabled="!canRun" class="flex h-7 items-center gap-1.5 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50" @click="run">
        <component :is="runIcon" class="size-3.5" />{{ runLabel }}
      </button>
    </div>
  </dialog>
</template>
