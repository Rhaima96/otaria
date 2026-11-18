<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type Charsets } from "@/lib/api";

/**
 * Create a database, or change an existing one's default character set and collation.
 * Server defaults (create) or the database's current values (alter) are preselected; the SQL is previewed.
 */
const props = defineProps<{ sessionId: string }>();
const emit = defineEmits<{ done: [name: string] }>();
const { t } = useI18n();

const dialog = ref<HTMLDialogElement | null>(null);
const mode = ref<"create" | "alter">("create");
const name = ref("");
const charset = ref("");
const collation = ref("");
const info = ref<Charsets | null>(null);
const sql = ref("");
const busy = ref(false);
const error = ref<AppError | null>(null);

const collations = computed(() => info.value?.charsets.find(([cs]) => cs === charset.value)?.[1] ?? []);

/** `schema` given: edit that database; else create a new one. */
async function open(schema?: string) {
  mode.value = schema ? "alter" : "create";
  name.value = schema ?? "";
  sql.value = "";
  error.value = null;
  dialog.value?.showModal();
  try {
    info.value = await api.listCharsets(props.sessionId);
    [charset.value, collation.value] = schema
      ? await api.databaseOptions(props.sessionId, schema)
      : [info.value.defaultCharset, info.value.defaultCollation];
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
    sql.value = await api.databaseSql(mode.value, name.value, charset.value, collation.value);
    error.value = null;
  } catch (e) {
    sql.value = "";
    error.value = toAppError(e);
  }
});

async function run() {
  busy.value = true;
  try {
    await api.databaseDdl(props.sessionId, mode.value, name.value, charset.value, collation.value);
    emit("done", name.value.trim());
    dialog.value?.close();
  } catch (e) {
    error.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

const field = "h-7 rounded-sm border bg-background px-2 read-only:text-muted-foreground";
const btn = "flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary disabled:opacity-50";
</script>

<template>
  <dialog ref="dialog" class="m-auto w-[min(32rem,94vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
    <form method="dialog" @submit.prevent="sql && !busy && run()">
      <div class="border-b px-4 py-2 font-semibold">{{ mode === "create" ? t("database.title") : t("database.editTitle", { name }) }}</div>
      <div class="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-2 p-4">
        <label for="db-name" class="text-muted-foreground">{{ t("database.name") }}</label>
        <input id="db-name" v-model="name" :readonly="mode === 'alter'" autofocus autocomplete="off" spellcheck="false" :class="field" />
        <label for="db-charset" class="text-muted-foreground">{{ t("database.charset") }}</label>
        <select id="db-charset" v-model="charset" :class="field" @change="pickCharset">
          <option v-for="[cs] in info?.charsets ?? []" :key="cs" :value="cs">{{ cs }}</option>
        </select>
        <label for="db-collation" class="text-muted-foreground">{{ t("database.collation") }}</label>
        <!-- keyed on the charset: the options must exist before the value is applied, or the select shows blank -->
        <select id="db-collation" :key="charset" v-model="collation" :class="field">
          <option v-for="c in collations" :key="c" :value="c">{{ c }}</option>
        </select>
        <p v-if="mode === 'alter'" class="col-span-2 text-muted-foreground">{{ t("database.alterHint") }}</p>
        <pre class="col-span-2 min-h-12 overflow-auto whitespace-pre-wrap rounded-sm border bg-card p-2 font-mono text-xs">{{ sql || t("database.preview") }}</pre>
        <ErrorLine v-if="error" :error="error" class="col-span-2" />
      </div>
      <div class="flex justify-end gap-2 border-t px-4 py-3">
        <button type="button" :class="btn" @click="dialog?.close()">{{ t("database.cancel") }}</button>
        <button type="submit" :disabled="!sql || busy" class="flex h-7 items-center gap-1.5 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50">
          {{ mode === "create" ? t("database.create") : t("database.apply") }}
        </button>
      </div>
    </form>
  </dialog>
</template>
