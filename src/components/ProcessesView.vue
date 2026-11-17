<script setup lang="ts">
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import { FileSearch, OctagonX, RefreshCw, Unplug } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type Process } from "@/lib/api";
import { INTERVALS, usePolling } from "@/lib/poll";
import { cn } from "@/lib/utils";

const props = defineProps<{ sessionId: string; active?: boolean }>();
const { t, locale } = useI18n();

const list = ref<Process[]>([]);
const error = ref<AppError | null>(null);
const filter = ref("");
const hideSleeping = ref(true);

const { every, tick } = usePolling(async () => {
  try {
    list.value = await api.listProcesses(props.sessionId);
    error.value = null;
  } catch (e) {
    error.value = toAppError(e);
  }
}, () => props.active !== false);

const shown = computed(() => {
  const f = filter.value.trim().toLowerCase();
  return list.value.filter(
    (p) => (!hideSleeping.value || p.command !== "Sleep") && (!f || `${p.id} ${p.user} ${p.host} ${p.db ?? ""} ${p.command} ${p.state ?? ""} ${p.info ?? ""}`.toLowerCase().includes(f)),
  );
});

const secs = computed(() => new Intl.NumberFormat(locale.value, { maximumFractionDigits: 1 }));

// KILL: confirm with the exact statement, then run it.
const killDialog = ref<HTMLDialogElement | null>(null);
const target = ref<{ id: number; connection: boolean } | null>(null);
const actionError = ref<AppError | null>(null);
const busy = ref(false);

function askKill(id: number, connection: boolean) {
  target.value = { id, connection };
  actionError.value = null;
  killDialog.value?.showModal();
}

async function kill() {
  const k = target.value!;
  busy.value = true;
  actionError.value = null;
  try {
    await api.killProcess(props.sessionId, k.id, k.connection);
    killDialog.value?.close();
    await tick();
  } catch (e) {
    actionError.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

// EXPLAIN FOR CONNECTION: plan of the running statement, as formatted JSON.
const planDialog = ref<HTMLDialogElement | null>(null);
const plan = ref("");
const planOf = ref<Process | null>(null);

async function explain(p: Process) {
  planOf.value = p;
  plan.value = "";
  actionError.value = null;
  planDialog.value?.showModal();
  try {
    const json = (await api.explainProcess(props.sessionId, p.id)) ?? "";
    try {
      plan.value = JSON.stringify(JSON.parse(json), null, 2);
    } catch {
      plan.value = json;
    }
  } catch (e) {
    actionError.value = toAppError(e);
  }
}

const cell = "border-b px-2 py-1 text-left align-top";
const btn = "flex h-6 items-center gap-1 rounded-sm px-1.5 text-muted-foreground hover:bg-secondary hover:text-foreground";
</script>

<template>
  <div class="flex flex-col">
    <div class="flex h-9 shrink-0 items-center gap-2 border-b px-3">
      <input v-model="filter" :placeholder="t('processes.filter')" :aria-label="t('processes.filter')" class="h-6 w-64 rounded-sm border bg-background px-2" />
      <label class="flex items-center gap-1.5 text-muted-foreground">
        <input v-model="hideSleeping" type="checkbox" class="size-3.5" />{{ t("processes.hideSleeping") }}
      </label>
      <label class="ml-auto flex items-center gap-1.5 text-muted-foreground">
        {{ t("admin.refreshEvery") }}
        <select v-model.number="every" class="h-6 rounded-sm border bg-background px-1">
          <option v-for="n in INTERVALS" :key="n" :value="n">{{ n ? t("admin.seconds", { n }) : t("admin.paused") }}</option>
        </select>
      </label>
      <button :class="btn" @click="tick"><RefreshCw class="size-3.5" />{{ t("admin.refresh") }}</button>
    </div>

    <ErrorLine v-if="error" :error="error" class="m-3" />

    <div class="min-h-0 flex-1 overflow-auto">
      <table class="w-full border-collapse">
        <thead class="sticky top-0 bg-sidebar text-muted-foreground">
          <tr>
            <th :class="cell">{{ t("processes.id") }}</th>
            <th :class="cell">{{ t("processes.user") }}</th>
            <th :class="cell">{{ t("processes.host") }}</th>
            <th :class="cell">{{ t("processes.db") }}</th>
            <th :class="cell">{{ t("processes.command") }}</th>
            <th :class="cn(cell, 'text-right')">{{ t("processes.time") }}</th>
            <th :class="cell">{{ t("processes.state") }}</th>
            <th :class="cell">{{ t("processes.info") }}</th>
            <th :class="cell"><span class="sr-only">{{ t("processes.actions") }}</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="p in shown" :key="p.id" class="hover:bg-secondary">
            <td :class="cn(cell, 'font-mono text-xs')">{{ p.id }}</td>
            <td :class="cell">{{ p.user }}</td>
            <td :class="cn(cell, 'font-mono text-xs')">{{ p.host }}</td>
            <td :class="cell">{{ p.db }}</td>
            <td :class="cell">{{ p.command }}</td>
            <td :class="cn(cell, 'text-right font-mono text-xs whitespace-nowrap')">{{ secs.format(p.timeMs / 1000) }}</td>
            <td :class="cell">
              {{ p.state }}
              <span v-if="p.progress > 0" class="text-muted-foreground">· {{ t("processes.progress", { n: secs.format(p.progress) }) }}</span>
            </td>
            <td :class="cn(cell, 'max-w-md font-mono text-xs')"><span class="line-clamp-2 break-all" :title="p.info ?? undefined">{{ p.info }}</span></td>
            <td :class="cn(cell, 'whitespace-nowrap')">
              <div class="flex gap-0.5">
                <button v-if="p.info && p.command === 'Query'" :class="btn" :title="t('processes.explain')" @click="explain(p)">
                  <FileSearch class="size-3.5" /><span class="sr-only">{{ t("processes.explain") }}</span>
                </button>
                <button v-if="p.info" :class="btn" :title="t('processes.killQuery')" @click="askKill(p.id, false)">
                  <OctagonX class="size-3.5" /><span class="sr-only">{{ t("processes.killQuery") }}</span>
                </button>
                <button :class="btn" :title="t('processes.killConnection')" @click="askKill(p.id, true)">
                  <Unplug class="size-3.5" /><span class="sr-only">{{ t("processes.killConnection") }}</span>
                </button>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
      <p v-if="!error && !shown.length" class="p-3 text-muted-foreground">{{ t("processes.none") }}</p>
    </div>

    <dialog ref="killDialog" class="m-auto w-[min(32rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <div class="border-b px-4 py-2 font-semibold">{{ t("grid.previewTitle") }}</div>
      <pre v-if="target" class="p-4 font-mono text-xs select-text">{{ `KILL ${target.connection ? "CONNECTION" : "QUERY"} ${target.id}` }}</pre>
      <p class="px-4 text-muted-foreground">{{ target?.connection ? t("processes.killConnectionHint") : t("processes.killQueryHint") }}</p>
      <ErrorLine v-if="actionError" :error="actionError" class="mx-4 mt-2" />
      <div class="flex justify-end gap-2 px-4 py-3">
        <button class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="killDialog?.close()">{{ t("grid.cancel") }}</button>
        <button :disabled="busy" class="h-7 rounded-sm bg-destructive px-3 font-semibold text-primary-foreground disabled:opacity-50" @click="kill">{{ t("grid.apply") }}</button>
      </div>
    </dialog>

    <dialog ref="planDialog" class="m-auto w-[min(56rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <div class="border-b px-4 py-2 font-semibold">{{ t("processes.planTitle", { id: planOf?.id }) }}</div>
      <pre class="max-h-[20vh] overflow-auto border-b px-4 py-2 font-mono text-xs whitespace-pre-wrap text-muted-foreground select-text">{{ planOf?.info }}</pre>
      <ErrorLine v-if="actionError" :error="actionError" class="mx-4 mt-2" />
      <pre class="max-h-[50vh] overflow-auto p-4 font-mono text-xs select-text">{{ plan || (actionError ? "" : t("processes.planLoading")) }}</pre>
      <div class="flex justify-end px-4 py-3">
        <button class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="planDialog?.close()">{{ t("object.close") }}</button>
      </div>
    </dialog>
  </div>
</template>
