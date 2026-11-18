<script setup lang="ts">
import { onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { Check, RefreshCw } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type Engine } from "@/lib/api";
import { cn } from "@/lib/utils";

// active: unused here, passed to every server screen (the polling ones pause when hidden).
const props = defineProps<{ sessionId: string; active?: boolean }>();
const { t } = useI18n();

const engines = ref<Engine[]>([]);
const error = ref<AppError | null>(null);

async function load() {
  error.value = null;
  try {
    engines.value = await api.engineList(props.sessionId);
  } catch (e) {
    error.value = toAppError(e);
  }
}
onMounted(load);

const cell = "border-b px-2 py-1 text-left align-top";
const btn = "flex h-6 items-center gap-1 rounded-sm px-2 text-muted-foreground hover:bg-secondary hover:text-foreground";
const supportClass = (s: string) => (s === "DEFAULT" ? "text-primary font-semibold" : s === "YES" ? "text-success" : "text-muted-foreground");
</script>

<template>
  <div class="flex flex-col">
    <div class="flex h-9 shrink-0 items-center gap-2 border-b px-3">
      <p class="text-muted-foreground">{{ t("engines.hint") }}</p>
      <button :class="cn(btn, 'ml-auto')" @click="load"><RefreshCw class="size-3.5" />{{ t("admin.refresh") }}</button>
    </div>

    <ErrorLine v-if="error" :error="error" class="m-3" />

    <div class="min-h-0 flex-1 overflow-auto">
      <table class="w-full border-collapse">
        <thead class="sticky top-0 bg-sidebar text-muted-foreground">
          <tr>
            <th :class="cell">{{ t("engines.name") }}</th>
            <th :class="cell">{{ t("engines.support") }}</th>
            <th :class="cell">{{ t("engines.transactions") }}</th>
            <th :class="cell">{{ t("engines.xa") }}</th>
            <th :class="cell">{{ t("engines.savepoints") }}</th>
            <th :class="cell">{{ t("engines.comment") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="e in engines" :key="e.name">
            <td :class="cn(cell, 'font-mono')">{{ e.name }}</td>
            <td :class="cn(cell, supportClass(e.support))">{{ t(`engines.supports.${e.support}`, e.support) }}</td>
            <td v-for="(f, i) in [e.transactions, e.xa, e.savepoints]" :key="i" :class="cell">
              <Check v-if="f" class="size-3.5 text-success" :aria-label="t('engines.yes')" />
            </td>
            <td :class="cn(cell, 'text-muted-foreground')">{{ e.comment }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>
