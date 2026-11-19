<script setup lang="ts">
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import { ChevronRight } from "lucide-vue-next";
import { cn } from "@/lib/utils";

/** One node of an EXPLAIN / ANALYZE FORMAT=JSON plan; `total` = whole query time, for the time bar. */
export interface Plan {
  label: string;
  scalars: Record<string, unknown>;
  children: Plan[];
}
const props = defineProps<{ node: Plan; total: number; depth?: number }>();
const { t, locale } = useI18n();
const open = ref((props.depth ?? 0) < 6);

const s = computed(() => props.node.scalars);
const num = (v: unknown) => (typeof v === "number" ? new Intl.NumberFormat(locale.value, { maximumFractionDigits: 2 }).format(v) : String(v));
/** Shown as their own badges; every other scalar is listed below. */
const MAIN = ["table_name", "access_type", "key", "rows", "r_rows", "filtered", "r_filtered", "r_total_time_ms", "r_table_time_ms", "r_other_time_ms", "r_loops", "attached_condition"];
const rest = computed(() => Object.entries(s.value).filter(([k]) => !MAIN.includes(k)));
/** Estimate off by 10× or more. */
const off = computed(() => {
  const e = Number(s.value.rows), a = Number(s.value.r_rows);
  return s.value.r_rows !== undefined && e >= 0 && a >= 0 && Math.max(e, a) >= 10 && Math.max(e, a) >= 10 * Math.max(Math.min(e, a), 1);
});
/** Table nodes split their time in two (engine + the rest). */
const time = computed(() => {
  const { r_total_time_ms: total, r_table_time_ms: table, r_other_time_ms: other } = s.value;
  if (typeof total === "number") return total;
  return typeof table === "number" ? table + (typeof other === "number" ? other : 0) : null;
});
</script>

<template>
  <div class="border-l pl-3">
    <div class="flex flex-wrap items-center gap-x-3 gap-y-0.5 py-1">
      <button v-if="node.children.length" :aria-expanded="open" class="-ml-5 flex size-4 items-center justify-center text-muted-foreground" @click="open = !open">
        <ChevronRight :class="cn('size-3.5 transition-transform', open && 'rotate-90')" />
      </button>
      <span class="font-semibold">{{ node.label }}</span>
      <span v-if="s.table_name !== undefined" class="font-mono text-primary">{{ s.table_name }}</span>
      <span v-if="s.access_type !== undefined" :class="cn('rounded-sm border px-1 font-mono text-xs', s.access_type === 'ALL' && 'border-warning text-warning')">{{ s.access_type }}</span>
      <span v-if="s.key !== undefined" class="text-muted-foreground">{{ t("plan.key") }} <span class="font-mono text-foreground">{{ s.key }}</span></span>
      <span v-if="s.rows !== undefined" :class="cn(off && 'text-warning')" :title="off ? t('plan.offHint') : undefined">
        {{ t("plan.rows") }} {{ num(s.rows) }}<template v-if="s.r_rows !== undefined"> → {{ num(s.r_rows) }}</template>
      </span>
      <span v-if="s.filtered !== undefined">{{ t("plan.filtered") }} {{ num(s.filtered) }} %<template v-if="s.r_filtered !== undefined"> → {{ num(s.r_filtered) }} %</template></span>
      <span v-if="s.r_loops !== undefined" class="text-muted-foreground">{{ t("plan.loops", { n: num(s.r_loops) }, Number(s.r_loops)) }}</span>
      <span v-if="time !== null" class="flex items-center gap-1.5">
        {{ t("plan.time", { ms: num(time) }) }}
        <span class="h-1.5 w-20 overflow-hidden rounded-full bg-secondary"><span class="block h-full bg-primary" :style="{ width: `${total > 0 ? Math.min(100, (time / total) * 100) : 0}%` }" /></span>
      </span>
    </div>
    <p v-if="s.attached_condition !== undefined" class="pb-1 font-mono text-xs break-all text-muted-foreground">{{ t("plan.condition") }} {{ s.attached_condition }}</p>
    <p v-if="rest.length" class="pb-1 text-xs text-muted-foreground">
      <span v-for="[k, v] in rest" :key="k" class="mr-3 inline-block">{{ k }} <span class="font-mono text-foreground">{{ num(v) }}</span></span>
    </p>
    <div v-if="open">
      <PlanNode v-for="(c, i) in node.children" :key="i" :node="c" :total="total" :depth="(depth ?? 0) + 1" />
    </div>
  </div>
</template>
