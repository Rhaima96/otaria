<script setup lang="ts">
import { nextTick, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { DropdownMenuContent, DropdownMenuItem, DropdownMenuPortal, DropdownMenuRoot, DropdownMenuTrigger } from "reka-ui";
import { ChevronRight, Database, Download, Ellipsis, GitCompareArrows, LoaderCircle, Network, Plus, RefreshCw, Search, Upload } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import DatabaseDialog from "@/components/DatabaseDialog.vue";
import { useConfirm } from "@/composables/useConfirm";
import { api, toAppError, type AppError, type DbObject, type ObjectKind } from "@/lib/api";
import { KIND_ORDER, objectIcon } from "@/lib/objects";
import { cn } from "@/lib/utils";

/** `mariadb`: offer MariaDB-only actions (new sequence); hidden on MySQL. */
const props = defineProps<{ sessionId: string; selected: string | null; mariadb: boolean }>();
const emit = defineEmits<{ open: [schema: string, obj: DbObject]; create: [schema: string, kind: ObjectKind]; transfer: [schema: string, mode: "export" | "import"]; diagram: [schema: string]; compare: [schema: string]; search: []; dropped: [schema: string] }>();
const creatable = (): ObjectKind[] => KIND_ORDER.filter((k) => k !== "sequence" || props.mariadb);
const { t } = useI18n();

const schemas = ref<string[]>([]);
/** Loaded objects per expanded schema; "loading" while the request runs. */
const expanded = ref<Record<string, DbObject[] | "loading">>({});
const error = ref<AppError | null>(null);
const dbDialog = ref<InstanceType<typeof DatabaseDialog> | null>(null);
const { confirm } = useConfirm();
// The server's own databases: no edit or drop offered.
const SYSTEM = ["information_schema", "performance_schema", "mysql", "sys"];

async function dropSchema(s: string) {
  if (!(await confirm(t("database.dropConfirm", { name: s }), t("database.drop")))) return;
  try {
    await api.databaseDdl(props.sessionId, "drop", s, "", "");
    delete expanded.value[s];
    emit("dropped", s);
    await load();
  } catch (e) {
    error.value = toAppError(e);
  }
}

async function load() {
  error.value = null;
  try {
    schemas.value = await api.listSchemas(props.sessionId);
    // Refresh the schemas that are open.
    for (const s of Object.keys(expanded.value)) await loadObjects(s);
  } catch (e) {
    error.value = toAppError(e);
  }
}

async function loadObjects(schema: string) {
  expanded.value[schema] = "loading";
  try {
    const objs = await api.listObjects(props.sessionId, schema);
    expanded.value[schema] = objs.sort((a, b) => KIND_ORDER.indexOf(a.kind) - KIND_ORDER.indexOf(b.kind) || a.name.localeCompare(b.name));
  } catch (e) {
    delete expanded.value[schema];
    error.value = toAppError(e);
  }
}

function toggle(schema: string) {
  if (expanded.value[schema]) delete expanded.value[schema];
  else loadObjects(schema);
}

/** Expand the object's schema and scroll its row into view (object opened from Cmd+P). */
const root = ref<HTMLElement | null>(null);
async function reveal(schema: string, obj: DbObject) {
  if (!Array.isArray(expanded.value[schema])) await loadObjects(schema);
  await nextTick();
  root.value?.querySelector(`[data-id="${CSS.escape(`${schema}.${obj.kind}.${obj.name}`)}"]`)?.scrollIntoView({ block: "nearest" });
}

onMounted(load);
defineExpose({ load, reveal });
const row = "flex w-full items-center gap-1.5 py-0.5 pr-2 text-left hover:bg-secondary";
// Row actions on hover. Transfer icons match the dialog: export = Download (to a file), import = Upload (a file).
const trigger =
  "absolute top-0 hidden size-5 items-center justify-center rounded-sm text-muted-foreground group-hover:flex hover:bg-background hover:text-foreground focus-visible:flex data-[state=open]:flex";
const menu = "z-50 min-w-40 rounded-md border bg-popover p-1 text-popover-foreground shadow-md";
const item = "cursor-default rounded-sm px-2 py-1 outline-none data-highlighted:bg-secondary";
</script>

<template>
  <div ref="root" class="flex h-full flex-col">
    <div class="flex h-8 shrink-0 items-center justify-between border-b px-2">
      <span class="font-medium text-muted-foreground">{{ t("tree.title") }}</span>
      <span class="flex items-center">
        <button :title="t('database.open')" class="flex size-6 items-center justify-center rounded-sm hover:bg-secondary" @click="dbDialog?.open()">
          <Plus class="size-3.5" /><span class="sr-only">{{ t("database.open") }}</span>
        </button>
        <button :title="t('palette.button')" class="flex size-6 items-center justify-center rounded-sm hover:bg-secondary" @click="emit('search')">
          <Search class="size-3.5" /><span class="sr-only">{{ t("palette.button") }}</span>
        </button>
        <button :title="t('tree.refresh')" class="flex size-6 items-center justify-center rounded-sm hover:bg-secondary" @click="load">
          <RefreshCw class="size-3.5" /><span class="sr-only">{{ t("tree.refresh") }}</span>
        </button>
      </span>
    </div>
    <ErrorLine v-if="error" :error="error" class="m-2" />
    <DatabaseDialog ref="dbDialog" :session-id="sessionId" @done="load" />
    <ul class="min-h-0 flex-1 overflow-auto py-1 select-none" role="tree">
      <li v-for="s in schemas" :key="s" role="treeitem" :aria-expanded="!!expanded[s]">
        <div class="group relative">
          <button :class="cn(row, 'pl-1')" @click="toggle(s)">
            <ChevronRight :class="cn('size-3.5 shrink-0 text-muted-foreground transition-transform', expanded[s] && 'rotate-90')" />
            <Database class="size-3.5 shrink-0 text-primary" />
            <span class="truncate">{{ s }}</span>
            <LoaderCircle v-if="expanded[s] === 'loading'" class="ml-auto size-3.5 shrink-0 animate-spin text-muted-foreground" />
          </button>
          <DropdownMenuRoot>
            <DropdownMenuTrigger :title="t('newObject.menu')" :class="cn(trigger, 'right-1')">
              <Plus class="size-3.5" /><span class="sr-only">{{ t("newObject.menu") }}</span>
            </DropdownMenuTrigger>
            <DropdownMenuPortal>
              <DropdownMenuContent align="start" :class="menu">
                <DropdownMenuItem v-for="k in creatable()" :key="k" :class="item" @select="emit('create', s, k)">
                  {{ t(`newObject.${k}`) }}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenuPortal>
          </DropdownMenuRoot>
          <button :title="t('transfer.importSql')" :class="cn(trigger, 'right-7')" @click="emit('transfer', s, 'import')">
            <Upload class="size-3.5" /><span class="sr-only">{{ t("transfer.importSql") }}</span>
          </button>
          <button :title="t('transfer.exportSchema')" :class="cn(trigger, 'right-13')" @click="emit('transfer', s, 'export')">
            <Download class="size-3.5" /><span class="sr-only">{{ t("transfer.exportSchema") }}</span>
          </button>
          <button :title="t('diagram.open')" :class="cn(trigger, 'right-19')" @click="emit('diagram', s)">
            <Network class="size-3.5" /><span class="sr-only">{{ t("diagram.open") }}</span>
          </button>
          <button :title="t('compare.open')" :class="cn(trigger, 'right-25')" @click="emit('compare', s)">
            <GitCompareArrows class="size-3.5" /><span class="sr-only">{{ t("compare.open") }}</span>
          </button>
          <DropdownMenuRoot v-if="!SYSTEM.includes(s)">
            <DropdownMenuTrigger :title="t('database.menu')" :class="cn(trigger, 'right-31')">
              <Ellipsis class="size-3.5" /><span class="sr-only">{{ t("database.menu") }}</span>
            </DropdownMenuTrigger>
            <DropdownMenuPortal>
              <DropdownMenuContent align="start" :class="menu">
                <DropdownMenuItem :class="item" @select="dbDialog?.open(s)">{{ t("database.edit") }}</DropdownMenuItem>
                <DropdownMenuItem :class="cn(item, 'text-destructive')" @select="dropSchema(s)">{{ t("database.drop") }}…</DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenuPortal>
          </DropdownMenuRoot>
        </div>
        <ul v-if="Array.isArray(expanded[s])" role="group">
          <li v-for="o in expanded[s]" :key="o.kind + o.name" role="treeitem">
            <button
              :data-id="`${s}.${o.kind}.${o.name}`"
              :class="cn(row, 'pl-9', selected === `${s}.${o.kind}.${o.name}` && 'bg-selection')"
              :title="t(`kinds.${o.kind}`)"
              @click="emit('open', s, o)"
            >
              <component :is="objectIcon(o)" class="size-3.5 shrink-0 text-muted-foreground" />
              <span class="truncate">{{ o.name }}</span>
            </button>
          </li>
          <li v-if="!expanded[s].length" class="py-0.5 pl-9 text-muted-foreground">{{ t("tree.empty") }}</li>
        </ul>
      </li>
    </ul>
  </div>
</template>
