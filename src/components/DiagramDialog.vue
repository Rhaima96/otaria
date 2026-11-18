<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { save } from "@tauri-apps/plugin-dialog";
import { Check, Copy, Save } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError } from "@/lib/api";

/** Mermaid `erDiagram` text for a schema: pick tables, then copy or save as .mmd. */
const props = defineProps<{ sessionId: string }>();
const { t } = useI18n();

const dialog = ref<HTMLDialogElement | null>(null);
const schema = ref("");
const tables = ref<string[]>([]);
const picked = ref<string[]>([]);
const neighbors = ref(false);
const filter = ref("");
const text = ref("");
const copied = ref(false);
const result = ref("");
const error = ref<AppError | null>(null);

const shown = computed(() => tables.value.filter((x) => x.toLowerCase().includes(filter.value.toLowerCase())));

async function open(s: string) {
  schema.value = s;
  tables.value = [];
  picked.value = [];
  neighbors.value = false;
  filter.value = "";
  text.value = "";
  result.value = "";
  error.value = null;
  dialog.value?.showModal();
  try {
    tables.value = (await api.listObjects(props.sessionId, s)).filter((o) => o.kind === "table").map((o) => o.name);
    picked.value = [...tables.value];
  } catch (e) {
    error.value = toAppError(e);
  }
}
defineExpose({ open });

// Regenerate on every change; a slower earlier answer never overwrites a newer one.
let seq = 0;
watch([picked, neighbors], async () => {
  const n = ++seq;
  copied.value = false;
  result.value = "";
  if (!picked.value.length) return (text.value = "");
  try {
    const out = await api.erDiagram(props.sessionId, schema.value, picked.value, neighbors.value);
    if (n === seq) (text.value = out), (error.value = null);
  } catch (e) {
    if (n === seq) error.value = toAppError(e);
  }
});

/** All / none act on the filtered list only. */
function pickShown(on: boolean) {
  const set = new Set(picked.value);
  for (const x of shown.value) on ? set.add(x) : set.delete(x);
  picked.value = tables.value.filter((x) => set.has(x));
}

async function copy() {
  await navigator.clipboard.writeText(text.value);
  copied.value = true;
}

async function saveFile() {
  const path = await save({ defaultPath: `${schema.value}.mmd`, filters: [{ name: "Mermaid", extensions: ["mmd"] }] });
  if (!path) return;
  try {
    await api.saveDiagram(path, text.value);
    result.value = t("diagram.saved", { path });
  } catch (e) {
    error.value = toAppError(e);
  }
}

const btn = "flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary disabled:opacity-50";
const field = "h-7 rounded-sm border bg-background px-2";
</script>

<template>
  <dialog ref="dialog" class="m-auto w-[min(60rem,94vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
    <div class="border-b px-4 py-2 font-semibold">{{ t("diagram.title", { schema }) }}</div>

    <div class="grid h-[65vh] grid-cols-[16rem_1fr] gap-3 p-4">
      <div class="flex min-h-0 flex-col gap-2">
        <input v-model="filter" :placeholder="t('diagram.filter')" :aria-label="t('diagram.filter')" :class="field" />
        <div class="flex items-center gap-2 text-muted-foreground">
          <button class="hover:text-foreground hover:underline" @click="pickShown(true)">{{ t("diagram.all") }}</button>
          <span>·</span>
          <button class="hover:text-foreground hover:underline" @click="pickShown(false)">{{ t("diagram.none") }}</button>
          <span class="ml-auto">{{ t("diagram.count", { n: picked.length, total: tables.length }) }}</span>
        </div>
        <ul class="min-h-0 flex-1 overflow-auto rounded-sm border p-1">
          <li v-for="x in shown" :key="x">
            <label class="flex items-center gap-1.5 rounded-sm px-1 py-0.5 hover:bg-secondary">
              <input v-model="picked" type="checkbox" :value="x" class="size-3.5" /><span class="truncate">{{ x }}</span>
            </label>
          </li>
          <li v-if="!tables.length && !error" class="px-1 text-muted-foreground">{{ t("diagram.noTables") }}</li>
        </ul>
        <label class="flex items-start gap-1.5"><input v-model="neighbors" type="checkbox" class="mt-0.5 size-3.5" />{{ t("diagram.neighbors") }}</label>
      </div>
      <div class="flex min-h-0 flex-col gap-2">
        <textarea :value="text" readonly :aria-label="t('diagram.title', { schema })" class="min-h-0 flex-1 resize-none rounded-sm border bg-card p-2 font-mono text-xs" />
        <p class="text-muted-foreground">{{ t("diagram.hint") }}</p>
        <ErrorLine v-if="error" :error="error" />
        <p v-if="result" role="status" class="break-all text-success">{{ result }}</p>
      </div>
    </div>

    <div class="flex justify-end gap-2 border-t px-4 py-3">
      <button :class="btn" @click="dialog?.close()">{{ t("diagram.close") }}</button>
      <button :disabled="!text" :class="btn" @click="copy">
        <component :is="copied ? Check : Copy" class="size-3.5" />{{ copied ? t("diagram.copied") : t("diagram.copy") }}
      </button>
      <button :disabled="!text" class="flex h-7 items-center gap-1.5 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50" @click="saveFile">
        <Save class="size-3.5" />{{ t("diagram.save") }}
      </button>
    </div>
  </dialog>
</template>
