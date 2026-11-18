<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type Charsets } from "@/lib/api";

/** New database: name, character set and collation (server defaults preselected), SQL preview, create. */
const props = defineProps<{ sessionId: string }>();
const emit = defineEmits<{ created: [name: string] }>();
const { t } = useI18n();

const dialog = ref<HTMLDialogElement | null>(null);
const name = ref("");
const charset = ref("");
const collation = ref("");
const info = ref<Charsets | null>(null);
const sql = ref("");
const busy = ref(false);
const error = ref<AppError | null>(null);

const collations = computed(() => info.value?.charsets.find(([cs]) => cs === charset.value)?.[1] ?? []);

async function open() {
  name.value = "";
  sql.value = "";
  error.value = null;
  dialog.value?.showModal();
  try {
    info.value = await api.listCharsets(props.sessionId);
    charset.value = info.value.defaultCharset;
    collation.value = info.value.defaultCollation;
  } catch (e) {
    error.value = toAppError(e);
  }
}
defineExpose({ open });

/** A charset picked by the user starts on its own default collation (listed first). */
function pickCharset() {
  collation.value = collations.value[0] ?? "";
}

watch([name, charset, collation], async () => {
  if (!name.value.trim()) return (sql.value = "");
  try {
    sql.value = await api.createDatabaseSql(name.value, charset.value, collation.value);
    error.value = null;
  } catch (e) {
    sql.value = "";
    error.value = toAppError(e);
  }
});

async function create() {
  busy.value = true;
  try {
    await api.createDatabase(props.sessionId, name.value, charset.value, collation.value);
    emit("created", name.value.trim());
    dialog.value?.close();
  } catch (e) {
    error.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

const field = "h-7 rounded-sm border bg-background px-2";
const btn = "flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary disabled:opacity-50";
</script>

<template>
  <dialog ref="dialog" class="m-auto w-[min(32rem,94vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
    <form method="dialog" @submit.prevent="sql && !busy && create()">
      <div class="border-b px-4 py-2 font-semibold">{{ t("newDatabase.title") }}</div>
      <div class="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-2 p-4">
        <label for="newdb-name" class="text-muted-foreground">{{ t("newDatabase.name") }}</label>
        <input id="newdb-name" v-model="name" autofocus autocomplete="off" spellcheck="false" :class="field" />
        <label for="newdb-charset" class="text-muted-foreground">{{ t("newDatabase.charset") }}</label>
        <select id="newdb-charset" v-model="charset" :class="field" @change="pickCharset">
          <option v-for="[cs] in info?.charsets ?? []" :key="cs" :value="cs">{{ cs }}</option>
        </select>
        <label for="newdb-collation" class="text-muted-foreground">{{ t("newDatabase.collation") }}</label>
        <!-- keyed on the charset: the options must exist before the value is applied, or the select shows blank -->
        <select id="newdb-collation" :key="charset" v-model="collation" :class="field">
          <option v-for="c in collations" :key="c" :value="c">{{ c }}</option>
        </select>
        <pre class="col-span-2 min-h-12 overflow-auto whitespace-pre-wrap rounded-sm border bg-card p-2 font-mono text-xs">{{ sql || t("newDatabase.preview") }}</pre>
        <ErrorLine v-if="error" :error="error" class="col-span-2" />
      </div>
      <div class="flex justify-end gap-2 border-t px-4 py-3">
        <button type="button" :class="btn" @click="dialog?.close()">{{ t("newDatabase.cancel") }}</button>
        <button type="submit" :disabled="!sql || busy" class="flex h-7 items-center gap-1.5 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50">
          {{ t("newDatabase.create") }}
        </button>
      </div>
    </form>
  </dialog>
</template>
