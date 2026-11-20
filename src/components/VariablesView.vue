<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { Lock, RefreshCw } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type Variable } from "@/lib/api";
import { cn } from "@/lib/utils";

const props = defineProps<{ sessionId: string; active?: boolean }>();
const { t } = useI18n();

const vars = ref<Variable[]>([]);
const error = ref<AppError | null>(null);
const filter = ref("");
const changedOnly = ref(false);

async function load() {
  error.value = null;
  try {
    vars.value = await api.listVariables(props.sessionId);
  } catch (e) {
    error.value = toAppError(e);
  }
}
onMounted(load);

/** Defaults and types come from SYSTEM_VARIABLES (MariaDB). MySQL has neither, only the value's source. */
const hasDefaults = computed(() => vars.value.some((v) => v.kind !== null));
const hasOrigin = computed(() => vars.value.some((v) => v.origin !== null));
const hasRange = computed(() => vars.value.some((v) => v.min !== null));
// MySQL: changed = set by something other than the compiled default (SET GLOBAL … = DEFAULT still counts).
const changed = (v: Variable) => (v.kind !== null ? v.value !== v.default : v.origin !== null && v.origin !== "COMPILED");
const range = (v: Variable) => (v.min !== null ? `${v.min} – ${v.max}` : "");

const shown = computed(() => {
  const f = filter.value.trim().toLowerCase();
  return vars.value.filter((v) => (!changedOnly.value || changed(v)) && (!f || `${v.name} ${v.value ?? ""} ${v.comment ?? ""}`.toLowerCase().includes(f)));
});

// SET GLOBAL: edit, live preview of the statement, apply.
const dialog = ref<HTMLDialogElement | null>(null);
const target = ref<Variable | null>(null);
/** null = back to the default. */
const draft = ref<string | null>(null);
const preview = ref("");
const previewError = ref<AppError | null>(null);
const applyError = ref<AppError | null>(null);
const busy = ref(false);

/** BOOLEAN, ENUM: pick from a list. SET / FLAGSET take several values, so they stay free text. */
const choices = computed(() => {
  const v = target.value;
  if (v?.kind === "BOOLEAN") return ["ON", "OFF"];
  return v?.kind === "ENUM" && v.choices ? v.choices.split(",") : null;
});

function edit(v: Variable) {
  if (v.readOnly) return;
  target.value = v;
  draft.value = v.value ?? "";
  applyError.value = null;
  dialog.value?.showModal();
}

watch([target, draft], async () => {
  if (!target.value) return;
  try {
    preview.value = await api.variableSql(target.value.name, target.value.kind, draft.value);
    previewError.value = null;
  } catch (e) {
    preview.value = "";
    previewError.value = toAppError(e);
  }
});

async function apply() {
  busy.value = true;
  applyError.value = null;
  try {
    await api.setVariable(props.sessionId, target.value!.name, draft.value);
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
</script>

<template>
  <div class="flex flex-col">
    <div class="flex h-9 shrink-0 items-center gap-2 border-b px-3">
      <input v-model="filter" :placeholder="t('variables.filter')" :aria-label="t('variables.filter')" class="h-6 w-64 rounded-sm border bg-background px-2" />
      <label v-if="hasOrigin" class="flex items-center gap-1.5 text-muted-foreground">
        <input v-model="changedOnly" type="checkbox" class="size-3.5" />{{ t("variables.changedOnly") }}
      </label>
      <button :class="cn(btn, 'ml-auto')" @click="load"><RefreshCw class="size-3.5" />{{ t("admin.refresh") }}</button>
    </div>
    <p class="border-b px-3 py-1.5 text-muted-foreground">{{ t("variables.hint") }}</p>

    <ErrorLine v-if="error" :error="error" class="m-3" />

    <div class="min-h-0 flex-1 overflow-auto">
      <table class="w-full border-collapse">
        <thead class="sticky top-0 bg-sidebar text-muted-foreground">
          <tr>
            <th :class="cell">{{ t("variables.name") }}</th>
            <th :class="cell">{{ t("variables.value") }}</th>
            <th v-if="hasDefaults" :class="cell">{{ t("variables.default") }}</th>
            <th v-if="hasOrigin" :class="cell">{{ t("variables.origin") }}</th>
            <th v-if="hasDefaults" :class="cell">{{ t("variables.type") }}</th>
            <th v-if="hasRange" :class="cell">{{ t("variables.range") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="v in shown"
            :key="v.name"
            :title="v.comment ?? undefined"
            :class="cn('hover:bg-secondary', !v.readOnly && 'cursor-pointer')"
            :tabindex="v.readOnly ? undefined : 0"
            @click="edit(v)"
            @keydown.enter="edit(v)"
          >
            <td :class="cn(cell, 'font-mono text-xs whitespace-nowrap')">
              <span class="flex items-center gap-1">
                {{ v.name }}
                <Lock v-if="v.readOnly" class="size-3 text-muted-foreground" :aria-label="t('variables.readOnly')" />
              </span>
            </td>
            <!-- Changed values: bold + an origin other than COMPILE-TIME / COMPILED, not color alone. -->
            <td :class="cn(cell, 'font-mono text-xs break-all', changed(v) && 'font-semibold text-primary')">{{ v.value }}</td>
            <td v-if="hasDefaults" :class="cn(cell, 'font-mono text-xs break-all text-muted-foreground')">{{ v.default }}</td>
            <td v-if="hasOrigin" :class="cn(cell, 'text-xs whitespace-nowrap')">{{ v.origin }}</td>
            <td v-if="hasDefaults" :class="cn(cell, 'text-xs whitespace-nowrap text-muted-foreground')">{{ v.kind }}</td>
            <td v-if="hasRange" :class="cn(cell, 'font-mono text-xs whitespace-nowrap text-muted-foreground')">{{ range(v) }}</td>
          </tr>
        </tbody>
      </table>
      <p v-if="!error && !shown.length" class="p-3 text-muted-foreground">{{ t("variables.none") }}</p>
    </div>

    <dialog ref="dialog" class="m-auto w-[min(40rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <form v-if="target" @submit.prevent="apply">
        <div class="border-b px-4 py-2 font-mono font-semibold">{{ target.name }}</div>
        <div class="space-y-2 p-4">
          <p v-if="target.comment" class="text-muted-foreground">{{ target.comment }}</p>
          <div class="flex items-center gap-2">
            <select v-if="choices && draft !== null" v-model="draft" :aria-label="t('variables.value')" class="h-7 flex-1 rounded-sm border bg-background px-1 font-mono">
              <option v-for="c in choices" :key="c" :value="c">{{ c }}</option>
            </select>
            <input v-else-if="draft !== null" v-model="draft" :aria-label="t('variables.value')" class="h-7 flex-1 rounded-sm border bg-background px-2 font-mono" />
            <span v-else class="flex-1 text-muted-foreground">{{ target.default !== null ? t("variables.toDefault", { value: target.default }) : t("variables.toDefaultUnknown") }}</span>
            <button v-if="draft !== null" type="button" :class="btn" @click="draft = null">{{ t("variables.reset") }}</button>
            <button v-else type="button" :class="btn" @click="draft = target.value ?? ''">{{ t("variables.editValue") }}</button>
          </div>
          <ErrorLine v-if="previewError" :error="previewError" />
          <pre v-else class="rounded-sm bg-muted p-2 font-mono text-xs whitespace-pre-wrap select-text">{{ preview }}</pre>
          <ErrorLine v-if="applyError" :error="applyError" />
        </div>
        <div class="flex justify-end gap-2 px-4 pb-3">
          <button type="button" class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="dialog?.close()">{{ t("grid.cancel") }}</button>
          <button type="submit" :disabled="busy || !!previewError" class="h-7 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50">{{ t("grid.apply") }}</button>
        </div>
      </form>
    </dialog>
  </div>
</template>
