<script setup lang="ts">
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import { RefreshCw } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError } from "@/lib/api";
import { INTERVALS, usePolling } from "@/lib/poll";

const props = defineProps<{ sessionId: string; active?: boolean }>();
const { t, locale } = useI18n();

const HISTORY = 60;
const s = ref<Record<string, string>>({});
const error = ref<AppError | null>(null);
const qps = ref<number[]>([]);
const conns = ref<number[]>([]);
let prev: { questions: number; at: number } | null = null;

const num = (k: string) => Number(s.value[k] ?? 0);
function push(a: number[], v: number) {
  a.push(v);
  if (a.length > HISTORY) a.shift();
}

const { every, tick } = usePolling(async () => {
  try {
    s.value = await api.serverStatus(props.sessionId);
    error.value = null;
  } catch (e) {
    error.value = toAppError(e);
    return;
  }
  // Rates need two samples; a counter going down means the server restarted.
  const questions = num("Questions");
  const at = performance.now();
  if (prev && questions >= prev.questions) push(qps.value, ((questions - prev.questions) * 1000) / (at - prev.at));
  prev = { questions, at };
  push(conns.value, num("Threads_connected"));
}, () => props.active !== false);

const fmt = computed(() => new Intl.NumberFormat(locale.value, { maximumFractionDigits: 1 }));
const pct = computed(() => new Intl.NumberFormat(locale.value, { style: "percent", maximumFractionDigits: 1 }));
function bytes(n: number) {
  const gb = n >= 1024 ** 3;
  return new Intl.NumberFormat(locale.value, { style: "unit", unit: gb ? "gigabyte" : "megabyte", maximumFractionDigits: 1 }).format(n / 1024 ** (gb ? 3 : 2));
}
const ratio = (a: number, b: number) => (b > 0 ? pct.value.format(a / b) : "—");

const uptime = computed(() => {
  const u = num("Uptime");
  return t("status.uptimeValue", { d: Math.floor(u / 86400), h: Math.floor((u % 86400) / 3600), m: Math.floor((u % 3600) / 60) });
});
const pool = computed(() => {
  const total = num("Innodb_buffer_pool_pages_total");
  return { size: total * num("Innodb_page_size"), used: total - num("Innodb_buffer_pool_pages_free"), total };
});

/** Polyline points in a 0–100 × 0–30 box, newest on the right, scaled to the series' own max. */
function spark(a: number[]) {
  const max = Math.max(1, ...a);
  return a.map((v, i) => `${100 - ((a.length - 1 - i) * 100) / (HISTORY - 1)},${30 - (v / max) * 28}`).join(" ");
}

const tile = "rounded-md border bg-card p-3";
const label = "text-xs text-muted-foreground";
const big = "mt-1 font-mono text-xl font-semibold";
const small = "mt-1 text-xs text-muted-foreground";
</script>

<template>
  <div class="flex flex-col">
    <div class="flex h-9 shrink-0 items-center gap-2 border-b px-3">
      <label class="ml-auto flex items-center gap-1.5 text-muted-foreground">
        {{ t("admin.refreshEvery") }}
        <select v-model.number="every" class="h-6 rounded-sm border bg-background px-1">
          <option v-for="n in INTERVALS" :key="n" :value="n">{{ n ? t("admin.seconds", { n }) : t("admin.paused") }}</option>
        </select>
      </label>
      <button class="flex h-6 items-center gap-1 rounded-sm px-2 text-muted-foreground hover:bg-secondary hover:text-foreground" @click="tick">
        <RefreshCw class="size-3.5" />{{ t("admin.refresh") }}
      </button>
    </div>

    <ErrorLine v-if="error" :error="error" class="m-3" />

    <div v-if="Object.keys(s).length" class="grid min-h-0 flex-1 auto-rows-min grid-cols-[repeat(auto-fill,minmax(14rem,1fr))] gap-3 overflow-auto p-3">
      <section :class="tile">
        <h3 :class="label">{{ t("status.qps") }}</h3>
        <p :class="big">{{ qps.length ? fmt.format(qps[qps.length - 1]) : "—" }}</p>
        <svg viewBox="0 0 100 30" preserveAspectRatio="none" class="mt-2 h-8 w-full text-primary" aria-hidden="true">
          <polyline :points="spark(qps)" fill="none" stroke="currentColor" stroke-width="1.5" vector-effect="non-scaling-stroke" />
        </svg>
        <p :class="small">{{ t("status.questionsTotal", { n: fmt.format(num("Questions")) }) }}</p>
      </section>

      <section :class="tile">
        <h3 :class="label">{{ t("status.connections") }}</h3>
        <p :class="big">{{ fmt.format(num("Threads_connected")) }} <span class="text-sm text-muted-foreground">/ {{ fmt.format(num("max_connections")) }}</span></p>
        <svg viewBox="0 0 100 30" preserveAspectRatio="none" class="mt-2 h-8 w-full text-primary" aria-hidden="true">
          <polyline :points="spark(conns)" fill="none" stroke="currentColor" stroke-width="1.5" vector-effect="non-scaling-stroke" />
        </svg>
        <p :class="small">{{ t("status.connectionsDetail", { running: fmt.format(num("Threads_running")), peak: fmt.format(num("Max_used_connections")) }) }}</p>
      </section>

      <section :class="tile">
        <h3 :class="label">{{ t("status.bufferPool") }}</h3>
        <p :class="big">{{ ratio(pool.used, pool.total) }}</p>
        <div class="mt-2 h-2 overflow-hidden rounded-full bg-muted" role="progressbar" :aria-valuenow="pool.used" aria-valuemin="0" :aria-valuemax="pool.total">
          <div class="h-full bg-primary" :style="{ width: `${pool.total ? (pool.used * 100) / pool.total : 0}%` }" />
        </div>
        <p :class="small">{{ t("status.bufferPoolDetail", { size: bytes(pool.size) }) }}</p>
      </section>

      <section :class="tile">
        <h3 :class="label">{{ t("status.hitRatio") }}</h3>
        <p :class="big">{{ ratio(num("Innodb_buffer_pool_read_requests") - num("Innodb_buffer_pool_reads"), num("Innodb_buffer_pool_read_requests")) }}</p>
        <p :class="small">{{ t("status.hitRatioDetail", { reads: fmt.format(num("Innodb_buffer_pool_reads")) }) }}</p>
      </section>

      <section :class="tile">
        <h3 :class="label">{{ t("status.tmpDisk") }}</h3>
        <p :class="big">{{ ratio(num("Created_tmp_disk_tables"), num("Created_tmp_tables")) }}</p>
        <p :class="small">{{ t("status.tmpDiskDetail", { disk: fmt.format(num("Created_tmp_disk_tables")), all: fmt.format(num("Created_tmp_tables")) }) }}</p>
      </section>

      <section :class="tile">
        <h3 :class="label">{{ t("status.uptime") }}</h3>
        <p :class="big">{{ uptime }}</p>
        <p :class="small">{{ t("status.sinceStart") }}</p>
      </section>
    </div>
  </div>
</template>
