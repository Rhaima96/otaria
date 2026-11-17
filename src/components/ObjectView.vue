<script setup lang="ts">
import { ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { KeyRound } from "lucide-vue-next";
import DataGrid from "@/components/DataGrid.vue";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type DbObject, type TableInfo } from "@/lib/api";
import { hasRows } from "@/lib/objects";
import { cn } from "@/lib/utils";

const props = defineProps<{ sessionId: string; schema: string; obj: DbObject }>();
const { t } = useI18n();

type View = "data" | "structure" | "ddl";
const views: View[] = hasRows(props.obj.kind) ? ["data", "structure", "ddl"] : ["ddl"];
const view = ref<View>(views[0]);

const info = ref<TableInfo | null>(null);
const ddl = ref<string | null>(null);
const error = ref<AppError | null>(null);

// Each view loads its data the first time it is shown.
watch(
  view,
  async (v) => {
    error.value = null;
    try {
      if (v !== "ddl" && !info.value) info.value = await api.tableInfo(props.sessionId, props.schema, props.obj.name);
      if (v === "ddl" && ddl.value === null) ddl.value = await api.objectDdl(props.sessionId, props.schema, props.obj.name, props.obj.kind);
    } catch (e) {
      error.value = toAppError(e);
    }
  },
  { immediate: true },
);

const subtab = "h-full border-b-2 px-3 font-medium";
const cell = "border-b px-2 py-1 text-left align-top";
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
        @click="view = v"
      >
        {{ t(`object.${v}`) }}
      </button>
    </div>

    <ErrorLine v-if="error" :error="error" class="m-3" />

    <DataGrid v-else-if="view === 'data' && info" :session-id="sessionId" :schema="schema" :table="obj.name" :info="info" class="min-h-0 flex-1" />

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

    <pre v-else-if="view === 'ddl' && ddl !== null" class="min-h-0 flex-1 overflow-auto p-3 font-mono text-xs select-text">{{ ddl }}</pre>
  </div>
</template>
