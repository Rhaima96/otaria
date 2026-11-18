<script setup lang="ts">
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import { Braces, Check } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import JsonNode from "@/components/JsonNode.vue";
import { api, toAppError, type AppError } from "@/lib/api";
import { cn } from "@/lib/utils";

/** JSON cell viewer / editor: tree (scalars editable in place) or raw text, plus a JSON_TABLE starter query. */
const props = defineProps<{ schema: string; table: string }>();
const emit = defineEmits<{ apply: [text: string]; sql: [sql: string] }>();
const { t } = useI18n();

const dialog = ref<HTMLDialogElement | null>(null);
const column = ref("");
const editable = ref(false);
const original = ref("");
const text = ref("");
const mode = ref<"tree" | "text">("tree");
const doc = ref<any>(null);
const error = ref<AppError | null>(null);

const parsed = computed<{ ok: true; value: unknown } | { ok: false; message: string }>(() => {
  try {
    return { ok: true, value: JSON.parse(text.value) };
  } catch (e) {
    return { ok: false, message: (e as Error).message };
  }
});

/**
 * JSON.parse turns numbers into doubles: 12345678901234567890 or 1.10 would come back changed.
 * When the text has such a number, only the text is edited (and saved exactly as typed).
 */
const lossy = computed(() =>
  [...text.value.matchAll(/"(?:[^"\\]|\\.)*"|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/g)].some((m) => m[0][0] !== '"' && String(Number(m[0])) !== m[0]),
);
/** The tree would show (and save) the rounded numbers: lossy documents are text only. */
const treeOk = computed(() => parsed.value.ok && !lossy.value);

function open(x: { column: string; value: string | null; editable: boolean }) {
  column.value = x.column;
  editable.value = x.editable;
  original.value = text.value = x.value ?? "";
  error.value = null;
  syncDoc();
  mode.value = treeOk.value && doc.value !== null && typeof doc.value === "object" ? "tree" : "text";
  dialog.value?.showModal();
}
defineExpose({ open });

function syncDoc() {
  doc.value = parsed.value.ok ? parsed.value.value : null;
}

/** Tree edit → text, in the cell's own style (pretty if it had line breaks, else compact). */
function fromTree() {
  text.value = JSON.stringify(doc.value, null, original.value.includes("\n") ? 2 : undefined);
}

function setMode(m: "tree" | "text") {
  if (m === "tree") syncDoc();
  mode.value = m;
}

const format = () => (text.value = JSON.stringify(JSON.parse(text.value), null, 2));

function apply() {
  emit("apply", text.value);
  dialog.value?.close();
}

async function jsonTable(path: (string | number)[], sample: unknown) {
  try {
    emit("sql", await api.jsonTableSql(props.schema, props.table, column.value, path, sample));
    dialog.value?.close();
  } catch (e) {
    error.value = toAppError(e);
  }
}

const btn = "flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary disabled:opacity-50";
const seg = "h-6 rounded-sm px-2 hover:bg-secondary";
</script>

<template>
  <dialog ref="dialog" class="m-auto w-[min(56rem,92vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
    <div class="flex items-center gap-2 border-b px-4 py-2">
      <Braces class="size-4 text-muted-foreground" />
      <span class="font-semibold">{{ t("json.title", { column }) }}</span>
      <span v-if="!editable" class="text-muted-foreground">· {{ t("json.readOnly") }}</span>
      <div class="ml-auto flex gap-1" role="tablist">
        <button role="tab" :aria-selected="mode === 'tree'" :disabled="!treeOk" :class="cn(seg, mode === 'tree' && 'bg-secondary', 'disabled:opacity-40')" @click="setMode('tree')">{{ t("json.tree") }}</button>
        <button role="tab" :aria-selected="mode === 'text'" :class="cn(seg, mode === 'text' && 'bg-secondary')" @click="setMode('text')">{{ t("json.text") }}</button>
      </div>
    </div>

    <div class="flex h-[60vh] flex-col gap-2 p-4">
      <div v-if="mode === 'tree' && treeOk" class="min-h-0 flex-1 overflow-auto rounded-sm border bg-card p-2">
        <JsonNode :name="null" :value="doc" :parent="null" :path="[]" :editable="editable" :depth="0" @change="fromTree" @table="jsonTable" />
      </div>
      <textarea
        v-else
        v-model="text"
        :readonly="!editable"
        spellcheck="false"
        :aria-label="t('json.title', { column })"
        class="min-h-0 flex-1 resize-none rounded-sm border bg-card p-2 font-mono text-xs"
      />
      <p v-if="!parsed.ok" class="text-destructive">{{ t("json.invalid", { message: parsed.message }) }}</p>
      <p v-else-if="lossy" class="text-warning">{{ t("json.lossy") }}</p>
      <ErrorLine v-if="error" :error="error" />
    </div>

    <div class="flex items-center justify-end gap-2 border-t px-4 py-3">
      <button v-if="mode === 'text' && editable" :disabled="!parsed.ok || lossy" :title="lossy ? t('json.lossy') : undefined" :class="cn(btn, 'mr-auto')" @click="format">{{ t("json.format") }}</button>
      <button :class="btn" @click="dialog?.close()">{{ editable ? t("grid.cancel") : t("object.close") }}</button>
      <button
        v-if="editable"
        :disabled="!parsed.ok || text === original"
        class="flex h-7 items-center gap-1.5 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50"
        @click="apply"
      >
        <Check class="size-3.5" />{{ t("json.apply") }}
      </button>
    </div>
  </dialog>
</template>
