<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { ListboxContent, ListboxFilter, ListboxItem, ListboxRoot } from "reka-ui";
import { Loader2, Search } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type DbObject, type InsideMatch, type SchemaObject } from "@/lib/api";
import { objectIcon } from "@/lib/objects";
import { searchObjects } from "@/lib/search";

/** Cmd+P / Ctrl+P: find any object of any database by name, or by column name / routine body, and open it. */
const props = defineProps<{ sessionId: string }>();
const emit = defineEmits<{ open: [schema: string, obj: DbObject] }>();
const { t } = useI18n();

const dialog = ref<HTMLDialogElement | null>(null);
const objects = ref<SchemaObject[]>([]);
const query = ref("");
const loading = ref(false);
const error = ref<AppError | null>(null);

/** Objects are read again at each opening: the tree may have changed meanwhile. */
async function open() {
  query.value = "";
  error.value = null;
  dialog.value?.showModal();
  loading.value = true;
  try {
    objects.value = await api.allObjects(props.sessionId);
  } catch (e) {
    error.value = toAppError(e);
  } finally {
    loading.value = false;
  }
}
defineExpose({ open });

const results = computed(() => searchObjects(objects.value, query.value));
const key = (o: SchemaObject) => `${o.schema}.${o.kind}.${o.name}`;

// Columns and routine bodies: asked to the server (LIKE), 250 ms after the last keystroke, 2 characters at least.
const BODIES_KEY = "otaria.paletteBodies";
const bodies = ref(localStorage.getItem(BODIES_KEY) === "1");
const inside = ref<InsideMatch[]>([]);
const insideKey = (m: InsideMatch) => `in:${key(m)}.${m.column}`;
let timer = 0;
let seq = 0;
watch([query, bodies], ([q, b]) => {
  localStorage.setItem(BODIES_KEY, b ? "1" : "0");
  clearTimeout(timer);
  const term = q.trim().split(/[\s.]+/).at(-1) ?? "";
  const n = ++seq;
  if (term.length < 2) return (inside.value = []);
  timer = window.setTimeout(async () => {
    try {
      const found = await api.searchInside(props.sessionId, term, b);
      if (n === seq) inside.value = found;
    } catch (e) {
      if (n === seq) error.value = toAppError(e);
    }
  }, 250);
});

function pick(k: unknown) {
  const o = results.value.find((r) => key(r) === k) ?? inside.value.find((m) => insideKey(m) === k);
  if (!o) return;
  dialog.value?.close();
  emit("open", o.schema, { name: o.name, kind: o.kind, versioned: o.versioned });
}
</script>

<template>
  <!-- Escape closes the native dialog; clicking the backdrop too. -->
  <dialog
    ref="dialog"
    class="m-auto mt-[12vh] w-[min(40rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40"
    :aria-label="t('palette.title')"
    @click.self="dialog?.close()"
  >
    <ListboxRoot highlight-on-hover @update:model-value="pick">
      <div class="flex items-center gap-2 border-b px-3">
        <Search class="size-4 shrink-0 text-muted-foreground" />
        <ListboxFilter v-model="query" auto-focus :placeholder="t('palette.placeholder')" :aria-label="t('palette.placeholder')" class="h-10 w-full bg-transparent outline-none" />
        <Loader2 v-if="loading" class="size-4 shrink-0 animate-spin text-muted-foreground" />
      </div>
      <ErrorLine v-if="error" :error="error" class="m-2" />
      <ListboxContent class="max-h-[50vh] overflow-auto py-1">
        <ListboxItem
          v-for="o in results"
          :key="key(o)"
          :value="key(o)"
          class="flex cursor-default items-center gap-2 px-3 py-1 data-highlighted:bg-selection"
        >
          <component :is="objectIcon(o)" class="size-3.5 shrink-0 text-muted-foreground" />
          <span class="truncate font-mono">{{ o.name }}</span>
          <span class="ml-auto shrink-0 text-muted-foreground">{{ o.schema }} · {{ t(`kinds.${o.kind}`) }}</span>
        </ListboxItem>
        <div v-if="inside.length" class="mt-1 border-t px-3 pt-1.5 pb-0.5 text-xs font-medium text-muted-foreground">{{ t("palette.inside") }}</div>
        <ListboxItem
          v-for="m in inside"
          :key="insideKey(m)"
          :value="insideKey(m)"
          class="flex cursor-default items-center gap-2 px-3 py-1 data-highlighted:bg-selection"
        >
          <component :is="objectIcon(m)" class="size-3.5 shrink-0 text-muted-foreground" />
          <span class="truncate font-mono">{{ m.name }}<span v-if="m.column" class="text-muted-foreground">.{{ m.column }}</span></span>
          <span class="ml-auto shrink-0 text-muted-foreground">{{ m.schema }} · {{ m.column ? t("palette.column") : t("palette.body") }}</span>
        </ListboxItem>
      </ListboxContent>
      <label class="flex items-center gap-1.5 border-t px-3 py-1.5 text-muted-foreground">
        <input v-model="bodies" type="checkbox" class="size-3.5" />{{ t("palette.searchBodies") }}
      </label>
      <p v-if="!loading && query.trim() && !results.length && !inside.length" class="px-3 pb-3 text-muted-foreground">{{ t("palette.none") }}</p>
      <p v-else-if="!query.trim()" class="px-3 pb-3 text-muted-foreground">{{ t("palette.hint", { n: objects.length }, objects.length) }}</p>
    </ListboxRoot>
  </dialog>
</template>
