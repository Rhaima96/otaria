<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { Download, RefreshCw } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type Plugin } from "@/lib/api";
import { cn } from "@/lib/utils";

const props = defineProps<{ sessionId: string }>();
const { t } = useI18n();

const plugins = ref<Plugin[]>([]);
const error = ref<AppError | null>(null);
const filter = ref("");
const availableOnly = ref(false);

async function load() {
  error.value = null;
  try {
    plugins.value = await api.listPlugins(props.sessionId);
  } catch (e) {
    error.value = toAppError(e);
  }
}
onMounted(load);

const shown = computed(() => {
  const f = filter.value.trim().toLowerCase();
  return plugins.value.filter(
    (p) => (!availableOnly.value || p.status === "NOT INSTALLED") && (!f || `${p.name} ${p.kind} ${p.library ?? ""} ${p.description}`.toLowerCase().includes(f)),
  );
});

// INSTALL: preview the statement, then run it.
const dialog = ref<HTMLDialogElement | null>(null);
const target = ref<Plugin | null>(null);
const preview = ref("");
const applyError = ref<AppError | null>(null);
const busy = ref(false);

async function ask(p: Plugin) {
  error.value = null;
  applyError.value = null;
  try {
    preview.value = await api.pluginInstallSql(p.name, p.library!);
    target.value = p;
    dialog.value?.showModal();
  } catch (e) {
    error.value = toAppError(e);
  }
}

async function install() {
  const p = target.value!;
  busy.value = true;
  applyError.value = null;
  try {
    await api.pluginInstall(props.sessionId, p.name, p.library!);
    dialog.value?.close();
    await load();
  } catch (e) {
    applyError.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

const cell = "border-b px-2 py-1 text-left align-top";
const btn = "flex h-6 items-center gap-1 rounded-sm px-2 text-muted-foreground hover:bg-secondary hover:text-foreground";
const statusClass = (s: string) => (s === "ACTIVE" ? "text-success" : s === "NOT INSTALLED" ? "text-muted-foreground" : "text-warning");
</script>

<template>
  <div class="flex flex-col">
    <div class="flex h-9 shrink-0 items-center gap-2 border-b px-3">
      <input v-model="filter" :placeholder="t('plugins.filter')" :aria-label="t('plugins.filter')" class="h-6 w-64 rounded-sm border bg-background px-2" />
      <label class="flex items-center gap-1.5 text-muted-foreground">
        <input v-model="availableOnly" type="checkbox" class="size-3.5" />{{ t("plugins.availableOnly") }}
      </label>
      <button :class="cn(btn, 'ml-auto')" @click="load"><RefreshCw class="size-3.5" />{{ t("plugins.refresh") }}</button>
    </div>
    <p class="border-b px-3 py-1.5 text-muted-foreground">{{ t("plugins.hint") }}</p>

    <ErrorLine v-if="error" :error="error" class="m-3" />

    <div class="min-h-0 flex-1 overflow-auto">
      <table class="w-full border-collapse">
        <thead class="sticky top-0 bg-sidebar text-muted-foreground">
          <tr>
            <th :class="cell">{{ t("plugins.name") }}</th>
            <th :class="cell">{{ t("plugins.type") }}</th>
            <th :class="cell">{{ t("plugins.status") }}</th>
            <th :class="cell">{{ t("plugins.maturity") }}</th>
            <th :class="cell">{{ t("plugins.library") }}</th>
            <th :class="cell">{{ t("plugins.description") }}</th>
            <th :class="cell"><span class="sr-only">{{ t("plugins.install") }}</span></th>
          </tr>
        </thead>
        <tbody>
          <!-- Same name can come from two libraries (pam: auth_pam.so, auth_pam_v1.so). -->
          <tr v-for="p in shown" :key="`${p.name}/${p.library}`"class="hover:bg-secondary">
            <td :class="cn(cell, 'font-medium')">{{ p.name }}</td>
            <td :class="cell">{{ p.kind }}</td>
            <td :class="cn(cell, 'font-mono text-xs whitespace-nowrap', statusClass(p.status))">{{ p.status }}</td>
            <td :class="cn(cell, p.maturity !== 'Stable' && 'text-warning')">{{ p.maturity }}</td>
            <td :class="cn(cell, 'font-mono text-xs')">{{ p.library ?? t("plugins.builtIn") }}</td>
            <td :class="cell">{{ p.description }}</td>
            <td :class="cell">
              <button v-if="p.status === 'NOT INSTALLED' && p.library" :class="btn" @click="ask(p)"><Download class="size-3.5" />{{ t("plugins.install") }}</button>
            </td>
          </tr>
        </tbody>
      </table>
      <p v-if="!error && !shown.length" class="p-3 text-muted-foreground">{{ t("plugins.none") }}</p>
    </div>

    <dialog ref="dialog" class="m-auto w-[min(48rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <div class="border-b px-4 py-2 font-semibold">{{ t("grid.previewTitle") }}</div>
      <pre class="max-h-[40vh] overflow-auto p-4 font-mono text-xs whitespace-pre-wrap select-text">{{ preview }}</pre>
      <p class="px-4 text-muted-foreground">{{ t("plugins.installHint") }}</p>
      <p v-if="target && target.maturity !== 'Stable'" class="px-4 pt-1 text-warning">{{ t("plugins.notStable", { maturity: target.maturity }) }}</p>
      <ErrorLine v-if="applyError" :error="applyError" class="mx-4 mt-2" />
      <div class="flex justify-end gap-2 px-4 py-3">
        <button class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="dialog?.close()">{{ t("grid.cancel") }}</button>
        <button :disabled="busy" class="h-7 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50" @click="install">{{ t("grid.apply") }}</button>
      </div>
    </dialog>
  </div>
</template>
