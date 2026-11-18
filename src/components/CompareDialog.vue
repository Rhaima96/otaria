<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { FileCode, GitCompareArrows, LoaderCircle } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type Comparison } from "@/lib/api";
import { useSessions } from "@/composables/useSessions";
import { cn } from "@/lib/utils";

/** Compare the tables of a source schema (any open connection) with `schema` here; the script opens in the editor. */
const props = defineProps<{ sessionId: string }>();
const emit = defineEmits<{ script: [schema: string, sql: string] }>();
const { t } = useI18n();
const { sessions } = useSessions();

const dialog = ref<HTMLDialogElement | null>(null);
const schema = ref("");
const source = ref("");
const schemas = ref<string[]>([]);
const sourceSchema = ref("");
const result = ref<Comparison | null>(null);
const busy = ref(false);
const error = ref<AppError | null>(null);

const sourceName = computed(() => sessions.value.find((s) => s.sessionId === source.value)?.profile.name ?? "");

function open(s: string) {
  schema.value = s;
  result.value = null;
  error.value = null;
  if (!sessions.value.some((x) => x.sessionId === source.value)) source.value = props.sessionId;
  dialog.value?.showModal();
  loadSchemas();
}
defineExpose({ open });

// Default source schema: another one than the target when comparing inside one connection.
async function loadSchemas() {
  schemas.value = [];
  try {
    schemas.value = await api.listSchemas(source.value);
    if (!schemas.value.includes(sourceSchema.value) || (source.value === props.sessionId && sourceSchema.value === schema.value))
      sourceSchema.value = schemas.value.find((x) => source.value !== props.sessionId || x !== schema.value) ?? "";
  } catch (e) {
    error.value = toAppError(e);
  }
}
watch(source, loadSchemas);
watch([source, sourceSchema], () => (result.value = null));

async function run() {
  busy.value = true;
  error.value = null;
  try {
    result.value = await api.compareSchemas(source.value, sourceSchema.value, props.sessionId, schema.value);
  } catch (e) {
    error.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

function openScript() {
  emit("script", schema.value, result.value!.sql);
  dialog.value?.close();
}

const badge = { create: "text-success", alter: "text-warning", drop: "text-destructive" };
const btn = "flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary disabled:opacity-50";
const field = "h-7 min-w-0 rounded-sm border bg-background px-1.5";
</script>

<template>
  <dialog ref="dialog" class="m-auto w-[min(60rem,94vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
    <div class="border-b px-4 py-2 font-semibold">{{ t("compare.title", { schema }) }}</div>

    <div class="flex flex-wrap items-end gap-3 p-4 pb-2">
      <label class="flex flex-col gap-1">
        <span class="text-muted-foreground">{{ t("compare.source") }}</span>
        <select v-model="source" :class="field">
          <option v-for="s in sessions" :key="s.sessionId" :value="s.sessionId">{{ s.profile.name }}</option>
        </select>
      </label>
      <label class="flex flex-col gap-1">
        <span class="text-muted-foreground">{{ t("compare.sourceSchema") }}</span>
        <select v-model="sourceSchema" :class="field">
          <option v-for="s in schemas" :key="s" :value="s">{{ s }}</option>
        </select>
      </label>
      <span class="pb-1.5 text-muted-foreground">→ {{ schema }}</span>
      <button :disabled="!sourceSchema || busy" :class="cn(btn, 'ml-auto')" @click="run">
        <component :is="busy ? LoaderCircle : GitCompareArrows" :class="cn('size-3.5', busy && 'animate-spin')" />{{ t("compare.run") }}
      </button>
    </div>
    <p class="px-4 text-muted-foreground">{{ t("compare.hint") }}</p>

    <div class="grid h-[55vh] grid-cols-[16rem_1fr] gap-3 p-4">
      <ul class="min-h-0 overflow-auto rounded-sm border p-1">
        <li v-for="d in result?.diffs" :key="d.name" class="flex items-center gap-2 px-1 py-0.5">
          <span :class="cn('w-20 shrink-0 text-xs font-medium', badge[d.status])">{{ t(`compare.${d.status}`) }}</span>
          <span class="truncate font-mono">{{ d.name }}</span>
        </li>
        <li v-if="result && !result.diffs.length" class="px-1 text-success">{{ t("compare.same", { source: `${sourceName}.${sourceSchema}` }) }}</li>
      </ul>
      <div class="flex min-h-0 flex-col gap-2">
        <textarea :value="result?.sql ?? ''" readonly :aria-label="t('compare.script')" class="min-h-0 flex-1 resize-none rounded-sm border bg-card p-2 font-mono text-xs" />
        <ErrorLine v-if="error" :error="error" />
      </div>
    </div>

    <div class="flex justify-end gap-2 border-t px-4 py-3">
      <button :class="btn" @click="dialog?.close()">{{ t("diagram.close") }}</button>
      <button :disabled="!result?.sql" class="flex h-7 items-center gap-1.5 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50" @click="openScript">
        <FileCode class="size-3.5" />{{ t("compare.openScript") }}
      </button>
    </div>
  </dialog>
</template>
