<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { AgGridVue } from "ag-grid-vue3";
import type { ColDef } from "ag-grid-community";
import { Loader2, Search } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type Rows, type TableInfo, type VectorColumn, type VectorDistance } from "@/lib/api";
import { theme, vectorText } from "@/lib/grid";
import { cn } from "@/lib/utils";

/** VECTOR columns of one table (MariaDB 11.7+): their index, and a k-nearest-neighbour search. */
const props = defineProps<{ sessionId: string; schema: string; table: string; info: TableInfo }>();
const { t } = useI18n();

const columns = ref<VectorColumn[]>([]);
const error = ref<AppError | null>(null);
const loaded = ref(false);

const column = ref("");
const distance = ref<VectorDistance>("euclidean");
const byRow = ref(false);
const probe = ref("");
const k = ref(10);
const busy = ref(false);
const result = ref<Rows | null>(null);
const searchError = ref<AppError | null>(null);

onMounted(async () => {
  try {
    columns.value = await api.vectorColumns(props.sessionId, props.schema, props.table);
    column.value = columns.value[0]?.name ?? "";
  } catch (e) {
    error.value = toAppError(e);
  } finally {
    loaded.value = true;
  }
});

const current = computed(() => columns.value.find((c) => c.name === column.value));
// The index serves only its own metric (server default: euclidean): start on it.
const indexDistance = computed(() => (current.value?.index ? ((current.value.index.distance ?? "euclidean") as VectorDistance) : null));
watch(indexDistance, (d) => d && (distance.value = d), { immediate: true });
// A vector and a key value are different inputs: switching mode starts from an empty field.
watch(byRow, () => (probe.value = ""));
const singleKey = computed(() => props.info.primaryKey.length === 1);

async function search() {
  busy.value = true;
  searchError.value = null;
  try {
    result.value = await api.vectorSearch(props.sessionId, props.schema, props.table, column.value, distance.value, { kind: byRow.value ? "row" : "text", value: probe.value.trim() }, k.value);
  } catch (e) {
    result.value = null;
    searchError.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

const vectorNames = computed(() => new Set(columns.value.map((c) => c.name)));
const colDefs = computed<ColDef<(string | null)[]>[]>(() =>
  (result.value?.columns ?? []).map((name, i) => ({
    colId: String(i),
    headerName: i === 0 ? t("vector.distance") : name,
    pinned: i === 0 ? "left" : undefined,
    valueGetter: (p) => p.data?.[i],
    valueFormatter: (p) => (p.value == null ? "NULL" : vectorNames.value.has(name) && i > 0 ? vectorText(p.value) : String(p.value)),
    cellClassRules: { "text-muted-foreground italic": (p) => p.value == null },
  })),
);

const h = "mb-2 flex items-center gap-1.5 font-semibold";
const card = "rounded-md border p-3";
const primary = "flex h-7 items-center gap-1.5 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50";
const field = "h-7 rounded-sm border bg-background px-2";
const cell = "border-b px-2 py-1 text-left align-top";
</script>

<template>
  <!-- relative: keeps the absolute sr-only labels inside the scroll area (else they stretch the window). -->
  <div class="relative flex min-h-0 flex-col gap-4 overflow-auto p-4">
    <ErrorLine v-if="error" :error="error" />
    <p v-else-if="!loaded" class="flex items-center gap-1.5 text-muted-foreground"><Loader2 class="size-3.5 animate-spin" />{{ t("vector.loading") }}</p>
    <p v-else-if="!columns.length" class="text-muted-foreground">{{ t("vector.none") }}</p>

    <template v-else>
      <section :class="cn(card, 'max-w-4xl')">
        <h3 :class="h">{{ t("vector.columns") }}</h3>
        <table class="w-full border-collapse">
          <thead class="text-muted-foreground">
            <tr>
              <th :class="cell">{{ t("structure.name") }}</th>
              <th :class="cell">{{ t("vector.dimensions") }}</th>
              <th :class="cell">{{ t("vector.index") }}</th>
              <th :class="cell">{{ t("vector.distance") }}</th>
              <th :class="cell" :title="t('vector.mHint')">{{ t("vector.m") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="c in columns" :key="c.name">
              <td :class="cn(cell, 'font-mono')">{{ c.name }}</td>
              <td :class="cell">{{ c.dimensions }}</td>
              <template v-if="c.index">
                <td :class="cn(cell, 'font-mono')">{{ c.index.name }}</td>
                <td :class="cell">{{ c.index.distance ?? t("vector.serverDefault") }}</td>
                <td :class="cell">{{ c.index.m ?? t("vector.serverDefault") }}</td>
              </template>
              <td v-else colspan="3" :class="cn(cell, 'text-muted-foreground')">{{ t("vector.noIndex") }}</td>
            </tr>
          </tbody>
        </table>
      </section>

      <section :class="cn(card, 'max-w-4xl')">
        <h3 :class="h"><Search class="size-4 text-primary" />{{ t("vector.search") }}</h3>
        <form class="flex flex-col gap-2" @submit.prevent="search">
          <div class="flex flex-wrap items-center gap-3">
            <label v-if="columns.length > 1" class="flex items-center gap-1.5">
              {{ t("vector.column") }}
              <select v-model="column" :class="field"><option v-for="c in columns" :key="c.name" :value="c.name">{{ c.name }}</option></select>
            </label>
            <label class="flex items-center gap-1.5">
              {{ t("vector.distance") }}
              <select v-model="distance" :class="field">
                <option value="euclidean">{{ t("vector.euclidean") }}</option>
                <option value="cosine">{{ t("vector.cosine") }}</option>
              </select>
            </label>
            <label class="flex items-center gap-1.5">
              {{ t("vector.k") }}
              <input v-model.number="k" type="number" min="1" max="1000" required :class="cn(field, 'w-20')" />
            </label>
          </div>
          <p v-if="indexDistance && indexDistance !== distance" class="text-warning">{{ t("vector.indexUnused", { distance: t(`vector.${indexDistance}`) }) }}</p>
          <p v-else-if="!current?.index" class="text-muted-foreground">{{ t("vector.fullScan") }}</p>

          <div class="flex flex-wrap items-center gap-3">
            <label class="flex items-center gap-1.5"><input v-model="byRow" type="radio" :value="false" />{{ t("vector.byText") }}</label>
            <label :class="cn('flex items-center gap-1.5', !singleKey && 'opacity-50')" :title="singleKey ? undefined : t('vector.needSingleKey')">
              <input v-model="byRow" type="radio" :value="true" :disabled="!singleKey" />{{ t("vector.byRow", { key: info.primaryKey[0] ?? "" }) }}
            </label>
          </div>
          <input v-if="byRow" v-model="probe" required :placeholder="info.primaryKey[0]" :aria-label="t('vector.byRow', { key: info.primaryKey[0] ?? '' })" :class="cn(field, 'w-64 font-mono')" />
          <textarea
            v-else
            v-model="probe"
            required
            rows="3"
            :placeholder="t('vector.textPlaceholder', current?.dimensions ?? 0)"
            :aria-label="t('vector.byText')"
            class="rounded-sm border bg-background px-2 py-1 font-mono text-xs"
          />
          <div class="flex items-center gap-2">
            <button type="submit" :disabled="busy || !probe.trim()" :class="primary">
              <Loader2 v-if="busy" class="size-3.5 animate-spin" />{{ t("vector.run") }}
            </button>
            <span v-if="result" class="text-muted-foreground">{{ t("editor.rows", result.rows.length) }}</span>
          </div>
          <ErrorLine v-if="searchError" :error="searchError" />
        </form>
      </section>

      <AgGridVue
        v-if="result"
        class="h-96 shrink-0"
        :theme="theme"
        :row-data="result.rows"
        :column-defs="colDefs"
        :default-col-def="{ resizable: true, sortable: true, minWidth: 60, maxWidth: 480 }"
        :locale-text="{ noRowsToShow: t('grid.noRows') }"
        :enable-cell-text-selection="true"
      />
    </template>
  </div>
</template>
