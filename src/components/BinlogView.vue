<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { AgGridVue } from "ag-grid-vue3";
import type { ColDef } from "ag-grid-community";
import { Loader2, RefreshCw } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type BinlogEvent, type BinlogState } from "@/lib/api";
import { theme } from "@/lib/grid";
import { cn } from "@/lib/utils";

/** Binary log reader (MariaDB only): files on the left, events of the chosen file, one page at a time. */
// active: unused here, passed to every server screen (the polling ones pause when hidden).
const props = defineProps<{ sessionId: string; active?: boolean }>();
const { t, locale } = useI18n();

const PAGE = 1000;
const st = ref<BinlogState | null>(null);
const file = ref("");
const events = ref<BinlogEvent[]>([]);
const more = ref(false);
const busy = ref(false);
const filter = ref("");
const error = ref<AppError | null>(null);

async function load() {
  error.value = null;
  try {
    st.value = await api.binlogState(props.sessionId);
    // Start on the file being written (the last one), or stay on the chosen one if it still exists.
    const names = st.value.files.map((f) => f.name);
    await open(names.includes(file.value) ? file.value : (names.at(-1) ?? ""));
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
    const page = await api.binlogEvents(props.sessionId, name, append ? events.value.length : 0, PAGE);
    file.value = name;
    events.value = append ? [...events.value, ...page] : page;
    more.value = page.length === PAGE;
  } catch (e) {
    error.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

const shown = computed(() => {
  const f = filter.value.trim().toLowerCase();
  return f ? events.value.filter((e) => `${e.kind} ${e.info}`.toLowerCase().includes(f)) : events.value;
});

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
      <button :class="cn(btn, 'ml-auto')" @click="load"><RefreshCw class="size-3.5" />{{ t("admin.refresh") }}</button>
    </div>

    <ErrorLine v-if="error" :error="error" class="m-3" />

    <div v-if="st" class="flex min-h-0 flex-1">
      <ul class="w-56 shrink-0 overflow-auto border-r py-1" :aria-label="t('binlog.files')">
        <li v-for="(f, i) in st.files" :key="f.name">
          <button
            :class="cn('flex w-full items-center justify-between gap-2 px-3 py-1 text-left hover:bg-secondary', f.name === file && 'bg-selection')"
            :title="i === st.files.length - 1 ? t('binlog.current') : undefined"
            @click="open(f.name)"
          >
            <span :class="cn('truncate font-mono', i === st.files.length - 1 && 'text-primary')">{{ f.name }}</span>
            <span class="shrink-0 text-muted-foreground">{{ size(f.size) }}</span>
          </button>
        </li>
      </ul>

      <div class="flex min-w-0 flex-1 flex-col">
        <AgGridVue
          class="min-h-0 flex-1"
          :theme="theme"
          :row-data="shown"
          :column-defs="colDefs"
          :default-col-def="{ resizable: true, sortable: true }"
          :locale-text="{ noRowsToShow: t('grid.noRows') }"
          :enable-cell-text-selection="true"
          :tooltip-show-delay="300"
        />
        <div class="flex h-7 shrink-0 items-center gap-2 border-t px-2 text-muted-foreground">
          <Loader2 v-if="busy" class="size-3.5 animate-spin" />
          <span class="shrink-0">{{ shown.length < events.length ? t("binlog.matching", { n: shown.length, total: events.length }) : t("binlog.loaded", { n: events.length }) }}</span>
          <button v-if="more" :disabled="busy" :class="btn" class="text-foreground" @click="open(file, true)">{{ t("binlog.more") }}</button>
          <span class="ml-auto min-w-0 truncate" :title="t('binlog.hint')">{{ t("binlog.hint") }}</span>
        </div>
      </div>
    </div>
  </div>
</template>
