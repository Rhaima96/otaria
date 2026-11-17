<script setup lang="ts">
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import { listen } from "@tauri-apps/api/event";
import { open as openFile, save } from "@tauri-apps/plugin-dialog";
import { Download, LoaderCircle, Upload } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type ExportFormat, type TransferProgress } from "@/lib/api";

/** One dialog for every transfer: export a table / view / schema, or import an SQL file into a schema. */
export interface TransferTarget {
  mode: "export" | "import";
  schema: string;
  /** Export of one table or view; null = whole schema. */
  table: string | null;
  /** Views have no SQL export of their rows. */
  view?: boolean;
}

const props = defineProps<{ sessionId: string }>();
const emit = defineEmits<{ imported: [schema: string] }>();
const { t } = useI18n();

const dialog = ref<HTMLDialogElement | null>(null);
const target = ref<TransferTarget>({ mode: "export", schema: "", table: null });
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

function open(x: TransferTarget) {
  target.value = x;
  format.value = formats.value[0];
  data.value = true;
  drop.value = false;
  progress.value = null;
  result.value = "";
  error.value = null;
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
  } catch (e) {
    error.value = toAppError(e);
  } finally {
    stop();
    running.value = false;
  }
}

async function runExport() {
  const ext = format.value;
  const path = await save({ defaultPath: `${name.value}.${ext}`, filters: [{ name: ext.toUpperCase(), extensions: [ext] }] });
  if (!path) return;
  const { schema, table } = target.value;
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
  emit("imported", schema);
}

const btn = "flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary disabled:opacity-50";
</script>

<template>
  <dialog
    ref="dialog"
    class="m-auto w-[min(36rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40"
    @cancel="(e) => running && e.preventDefault()"
  >
    <div class="border-b px-4 py-2 font-semibold">
      {{ target.mode === "export" ? t("transfer.exportTitle", { name }) : t("transfer.importTitle", { name }) }}
    </div>

    <div class="space-y-3 p-4">
      <template v-if="target.mode === 'export'">
        <fieldset v-if="formats.length > 1" :disabled="running" class="flex flex-wrap gap-3">
          <legend class="mb-1 text-muted-foreground">{{ t("transfer.format") }}</legend>
          <label v-for="f in formats" :key="f" class="flex items-center gap-1.5">
            <input v-model="format" type="radio" :value="f" class="size-3.5" />{{ t(`transfer.formats.${f}`) }}
          </label>
        </fieldset>
        <div v-if="format === 'sql'" class="flex flex-col gap-1.5">
          <label class="flex items-center gap-1.5"><input v-model="data" :disabled="running" type="checkbox" class="size-3.5" />{{ t("transfer.withData") }}</label>
          <label class="flex items-center gap-1.5"><input v-model="drop" :disabled="running" type="checkbox" class="size-3.5" />{{ t("transfer.withDrop") }}</label>
          <p class="text-muted-foreground">{{ t("transfer.sqlHint") }}</p>
        </div>
      </template>
      <p v-else class="text-muted-foreground">{{ t("transfer.importHint", { schema: target.schema }) }}</p>

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
        <pre v-if="error.detail" class="max-h-40 overflow-auto rounded-sm border bg-card p-2 font-mono text-xs whitespace-pre-wrap select-text">{{ error.detail }}</pre>
      </template>
    </div>

    <div class="flex justify-end gap-2 px-4 py-3">
      <button v-if="running" :class="btn" @click="api.cancelJob(sessionId, jobId)">{{ t("transfer.stop") }}</button>
      <button v-else :class="btn" @click="dialog?.close()">{{ t("transfer.close") }}</button>
      <button
        :disabled="running"
        class="flex h-7 items-center gap-1.5 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50"
        @click="target.mode === 'export' ? runExport() : runImport()"
      >
        <component :is="target.mode === 'export' ? Download : Upload" class="size-3.5" />
        {{ target.mode === "export" ? t("transfer.chooseSave") : t("transfer.chooseOpen") }}
      </button>
    </div>
  </dialog>
</template>
