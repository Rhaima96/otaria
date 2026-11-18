<script setup lang="ts">
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import { ChevronRight, TableProperties } from "lucide-vue-next";
import { cn } from "@/lib/utils";

type Json = null | boolean | number | string | Json[] | { [k: string]: Json };
/** One key of a JSON document. Scalars are edited in place (`parent[name] = …`), then `change` is emitted. */
const props = defineProps<{ name: string | number | null; value: Json; parent: Json[] | Record<string, Json> | null; path: (string | number)[]; editable: boolean; depth: number }>();
const emit = defineEmits<{ change: []; table: [path: (string | number)[], sample: Json] }>();
const { t } = useI18n();
const open = ref(props.depth < 2);

const isObj = computed(() => props.value !== null && typeof props.value === "object");
const entries = computed(() => Object.entries(props.value as object) as [string, Json][]);
/** An array of objects: offer a JSON_TABLE query over it. */
const tableable = computed(() => Array.isArray(props.value) && props.value[0] !== null && typeof props.value[0] === "object" && !Array.isArray(props.value[0]));

const shown = (v: Json) => (typeof v === "string" ? v : JSON.stringify(v));
const color = (v: Json) => (v === null ? "text-muted-foreground italic" : typeof v === "string" ? "text-success" : typeof v === "boolean" ? "text-warning" : "text-primary");

/** Strings stay strings; other scalars are retyped from what was entered (true, 3, null…). */
function set(raw: string) {
  let v: Json = raw;
  if (typeof props.value !== "string") {
    try {
      v = JSON.parse(raw);
    } catch {
      /* keep it as a string */
    }
  }
  (props.parent as Record<string, Json>)[props.name as string] = v;
  emit("change");
}
const summary = computed(() => (Array.isArray(props.value) ? `[${entries.value.length}]` : `{${entries.value.length}}`));
const label = computed(() => (props.name === null ? "" : typeof props.name === "number" ? `[${props.name}]` : props.name));
</script>

<template>
  <div>
    <div class="flex items-center gap-1.5 py-px" :style="{ paddingLeft: `${depth}rem` }">
      <button v-if="isObj" :aria-expanded="open" class="flex size-4 items-center justify-center text-muted-foreground" @click="open = !open">
        <ChevronRight :class="cn('size-3.5 transition-transform', open && 'rotate-90')" />
      </button>
      <span v-else class="w-4" />
      <span v-if="label !== ''" class="shrink-0 font-mono text-muted-foreground">{{ label }}</span>
      <template v-if="isObj">
        <span class="font-mono text-xs text-muted-foreground">{{ summary }}</span>
        <button v-if="tableable" :title="t('json.tableHint')" class="ml-1 flex h-5 items-center gap-1 rounded-sm px-1 text-xs text-muted-foreground hover:bg-secondary hover:text-foreground" @click="emit('table', path, (value as Json[])[0])">
          <TableProperties class="size-3" />{{ t("json.table") }}
        </button>
      </template>
      <input
        v-else-if="editable && parent"
        :value="shown(value)"
        :aria-label="String(label)"
        :class="cn('h-5 min-w-0 flex-1 rounded-sm border border-transparent bg-transparent px-1 font-mono hover:border-border focus:border-border', color(value))"
        @change="set(($event.target as HTMLInputElement).value)"
      />
      <span v-else :class="cn('min-w-0 font-mono break-all', color(value))">{{ shown(value) }}</span>
    </div>
    <template v-if="isObj && open">
      <JsonNode
        v-for="[k, v] in entries"
        :key="k"
        :name="Array.isArray(value) ? Number(k) : k"
        :value="v"
        :parent="value as Record<string, Json>"
        :path="[...path, Array.isArray(value) ? Number(k) : k]"
        :editable="editable"
        :depth="depth + 1"
        @change="emit('change')"
        @table="(p, s) => emit('table', p, s)"
      />
    </template>
  </div>
</template>
