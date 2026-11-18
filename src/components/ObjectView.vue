<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { ask } from "@tauri-apps/plugin-dialog";
import { Download, Eraser, KeyRound, Pencil, Trash2, Upload } from "lucide-vue-next";
import DataGrid from "@/components/DataGrid.vue";
import ErrorLine from "@/components/ErrorLine.vue";
import SequenceView from "@/components/SequenceView.vue";
import TableDesigner from "@/components/TableDesigner.vue";
import TransferDialog from "@/components/TransferDialog.vue";
import VectorView from "@/components/VectorView.vue";
import VersioningView from "@/components/VersioningView.vue";
import { api, toAppError, type AppError, type DbObject, type TableInfo } from "@/lib/api";
import { hasRows } from "@/lib/objects";
import { editScript } from "@/lib/templates";
import { cn } from "@/lib/utils";

const props = defineProps<{ sessionId: string; schema: string; obj: DbObject; mariadb: boolean }>();
const emit = defineEmits<{ renamed: [name: string]; dropped: []; editSql: [sql: string]; treeChanged: []; versioned: [on: boolean] }>();
const { t } = useI18n();

type View = "sequence" | "data" | "structure" | "versioning" | "vector" | "ddl";
// A sequence's "rows" are its state: shown by the Sequence panel instead of a grid.
// Temporal features (versioning, periods) are MariaDB-only: no tab on MySQL.
const baseViews: View[] =
  props.obj.kind === "sequence"
    ? ["sequence", "ddl"]
    : props.obj.kind === "table" && props.mariadb
      ? ["data", "structure", "versioning", "ddl"]
      : hasRows(props.obj.kind)
        ? ["data", "structure", "ddl"]
        : ["ddl"];
const view = ref<View>(baseViews[0]);
const isTable = props.obj.kind === "table";

const info = ref<TableInfo | null>(null);
// Vectors tab: MariaDB tables with a VECTOR column, known once the grid has loaded the columns.
const hasVector = ref(false);
watch(info, (i) => i && (hasVector.value = i.columns.some((c) => c.dataType === "vector")));
const views = computed<View[]>(() => (hasVector.value && props.mariadb && isTable ? [...baseViews.slice(0, -1), "vector", "ddl"] : baseViews));
const transfer = ref<InstanceType<typeof TransferDialog> | null>(null);
const ddl = ref<string | null>(null);
const error = ref<AppError | null>(null);
// Unsaved grid rows or designer changes: SessionView asks before closing the tab.
const grid = ref<InstanceType<typeof DataGrid> | null>(null);
const designer = ref<InstanceType<typeof TableDesigner> | null>(null);
const dirty = computed(() => !!(grid.value?.dirty || designer.value?.dirty));
defineExpose({ dirty });
/** Views are v-if: leaving the grid or the designer drops their unsaved changes, so ask first. */
async function switchView(v: View) {
  if (v === view.value || (dirty.value && !(await ask(t("object.leaveDirty"), { kind: "warning", okLabel: t("object.leave"), cancelLabel: t("grid.cancel") })))) return;
  view.value = v;
}
/** Bumped after a structure change: remounts the grid on the new columns. */
const version = ref(0);

// Each view loads its data the first time it is shown (tables use the designer for Structure).
async function show(v: View) {
  error.value = null;
  try {
    if ((v === "data" || v === "vector" || (v === "structure" && !isTable)) && !info.value) info.value = await api.tableInfo(props.sessionId, props.schema, props.obj.name);
    if (v === "ddl" && ddl.value === null) ddl.value = await api.objectDdl(props.sessionId, props.schema, props.obj.name, props.obj.kind);
  } catch (e) {
    error.value = toAppError(e);
  }
}
watch(view, show, { immediate: true });

function saved(name: string) {
  info.value = null;
  ddl.value = null;
  version.value++;
  if (name !== props.obj.name) emit("renamed", name);
  else show(view.value);
}

/** Versioning switched on or off: new columns for the grid and DDL, new icon in the tree. */
function versionedChanged(on: boolean) {
  info.value = null;
  ddl.value = null;
  version.value++;
  emit("versioned", on);
}

// Drop / truncate: strong confirmation, the object's name must be typed.
const dialog = ref<HTMLDialogElement | null>(null);
const truncate = ref(false);
const sql = ref("");
const typed = ref("");
const dropError = ref<AppError | null>(null);

async function askDrop(trunc: boolean) {
  truncate.value = trunc;
  typed.value = "";
  dropError.value = null;
  try {
    sql.value = await api.dropSql(props.schema, props.obj.name, props.obj.kind, trunc);
    dialog.value?.showModal();
  } catch (e) {
    error.value = toAppError(e);
  }
}

async function confirmDrop() {
  dropError.value = null;
  try {
    await api.dropObject(props.sessionId, props.schema, props.obj.name, props.obj.kind, truncate.value);
    dialog.value?.close();
    if (truncate.value) saved(props.obj.name);
    else emit("dropped");
  } catch (e) {
    dropError.value = toAppError(e);
  }
}

const subtab = "h-full border-b-2 px-3 font-medium";
const cell = "border-b px-2 py-1 text-left align-top";
const btn = "flex h-6 items-center gap-1 rounded-sm px-2 text-muted-foreground hover:bg-secondary hover:text-foreground";
</script>

<template>
  <div class="flex h-full flex-col">
    <div class="flex h-8 shrink-0 items-stretch gap-1 border-b px-2" role="tablist">
      <button
        v-for="v in views"
        :key="v"
        role="tab"
        :aria-selected="view === v"
        :class="cn(subtab, view === v ? 'border-primary text-foreground' : 'border-transparent text-muted-foreground hover:text-foreground')"
        @click="switchView(v)"
      >
        {{ t(`object.${v}`) }}
      </button>
      <div class="ml-auto flex items-center gap-1">
        <button v-if="hasRows(obj.kind) && obj.kind !== 'sequence'" :class="btn" @click="transfer?.open({ mode: 'export', schema, table: obj.name, view: !isTable })">
          <Download class="size-3.5" />{{ t("transfer.export") }}
        </button>
        <button v-if="isTable" :class="btn" @click="transfer?.open({ mode: 'csv', schema, table: obj.name })"><Upload class="size-3.5" />{{ t("transfer.importCsv") }}</button>
        <button v-if="isTable" :class="btn" @click="askDrop(true)"><Eraser class="size-3.5" />{{ t("object.truncate") }}</button>
        <button :class="cn(btn, 'hover:text-destructive')" @click="askDrop(false)"><Trash2 class="size-3.5" />{{ t("object.drop") }}</button>
      </div>
    </div>

    <ErrorLine v-if="error" :error="error" class="m-3" />

    <SequenceView v-else-if="view === 'sequence'" :session-id="sessionId" :schema="schema" :name="obj.name" class="min-h-0 flex-1" />

    <DataGrid v-else-if="view === 'data' && info" ref="grid" :key="version" :session-id="sessionId" :schema="schema" :table="obj.name" :info="info" :versioned="obj.versioned" class="min-h-0 flex-1" @edit-sql="(sql) => emit('editSql', sql)" />

    <VersioningView
      v-else-if="view === 'versioning'"
      :session-id="sessionId"
      :schema="schema"
      :table="obj.name"
      class="min-h-0 flex-1"
      @changed="versionedChanged"
      @edit-sql="(sql) => emit('editSql', sql)"
    />

    <VectorView v-else-if="view === 'vector' && info" :key="version" :session-id="sessionId" :schema="schema" :table="obj.name" :info="info" class="min-h-0 flex-1" />

    <TableDesigner v-else-if="view === 'structure' && isTable" ref="designer" :session-id="sessionId" :schema="schema" :table="obj.name" :mariadb="mariadb" class="min-h-0 flex-1" @saved="saved" />

    <div v-else-if="view === 'structure' && info" class="min-h-0 flex-1 overflow-auto">
      <table class="w-full border-collapse">
        <thead class="sticky top-0 bg-sidebar text-muted-foreground">
          <tr>
            <th :class="cell">{{ t("structure.name") }}</th>
            <th :class="cell">{{ t("structure.type") }}</th>
            <th :class="cell">{{ t("structure.nullable") }}</th>
            <th :class="cell">{{ t("structure.default") }}</th>
            <th :class="cell">{{ t("structure.extra") }}</th>
            <th :class="cell">{{ t("structure.comment") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="c in info.columns" :key="c.name" class="hover:bg-secondary">
            <td :class="cn(cell, 'font-medium')">
              <span class="flex items-center gap-1.5">
                <KeyRound v-if="info.primaryKey.includes(c.name)" class="size-3.5 text-warning" :aria-label="t('structure.primaryKey')" />
                {{ c.name }}
              </span>
            </td>
            <td :class="cn(cell, 'font-mono')">{{ c.columnType }}</td>
            <td :class="cell">{{ c.nullable ? t("structure.yes") : t("structure.no") }}</td>
            <!-- MariaDB gives 'NULL' (a string) for DEFAULT NULL, and SQL NULL when there is no default. -->
            <td :class="cn(cell, 'font-mono')">{{ c.default }}</td>
            <td :class="cn(cell, 'font-mono')">{{ c.extra }}</td>
            <td :class="cell">{{ c.comment }}</td>
          </tr>
        </tbody>
      </table>
    </div>

    <template v-else-if="view === 'ddl' && ddl !== null">
      <div v-if="!hasRows(obj.kind)" class="flex h-8 shrink-0 items-center border-b px-2">
        <button :class="btn" @click="emit('editSql', editScript(obj.kind, schema, obj.name, ddl, mariadb))">
          <Pencil class="size-3.5" />{{ t("object.editInEditor") }}
        </button>
      </div>
      <pre class="min-h-0 flex-1 overflow-auto p-3 font-mono text-xs select-text">{{ ddl }}</pre>
    </template>

    <dialog ref="dialog" class="m-auto w-[min(36rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <form @submit.prevent="confirmDrop">
        <div class="border-b px-4 py-2 font-semibold text-destructive">{{ truncate ? t("object.truncateTitle") : t("object.dropTitle") }}</div>
        <pre class="p-4 font-mono text-xs whitespace-pre-wrap select-text">{{ sql }};</pre>
        <p class="px-4">{{ truncate ? t("object.truncateText") : t("object.dropText") }}</p>
        <label class="flex flex-col gap-1 px-4 pt-3">
          <span class="text-muted-foreground">{{ t("object.typeName", { name: obj.name }) }}</span>
          <input v-model="typed" autocomplete="off" class="h-7 rounded-sm border bg-background px-2 font-mono text-xs" />
        </label>
        <ErrorLine v-if="dropError" :error="dropError" class="mx-4 mt-2" />
        <div class="flex justify-end gap-2 px-4 py-3">
          <button type="button" class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="dialog?.close()">{{ t("grid.cancel") }}</button>
          <button type="submit" :disabled="typed !== obj.name" class="h-7 rounded-sm bg-destructive px-3 font-semibold text-primary-foreground disabled:opacity-40">
            {{ truncate ? t("object.truncate") : t("object.drop") }}
          </button>
        </div>
      </form>
    </dialog>
    <!-- A CSV import changed the rows: reload the grid. A copy may have created a database: reload the tree. -->
    <TransferDialog ref="transfer" :session-id="sessionId" @imported="(_, mode) => (mode === 'csv' ? saved(obj.name) : emit('treeChanged'))" />
  </div>
</template>
