<script setup lang="ts">
import { defineAsyncComponent, ref } from "vue";
import { useI18n } from "vue-i18n";
import { Activity, FileCode, Gauge, Info, ListPlus, Lock, LockOpen, Plus, Puzzle, SlidersHorizontal, TableProperties, TriangleAlert, X } from "lucide-vue-next";
import ObjectTree from "@/components/ObjectTree.vue";
import ObjectView from "@/components/ObjectView.vue";
// Monaco is heavy: load the editor on first use.
const SqlEditor = defineAsyncComponent(() => import("@/components/SqlEditor.vue"));
import PluginsView from "@/components/PluginsView.vue";
import ProcessesView from "@/components/ProcessesView.vue";
import StatusView from "@/components/StatusView.vue";
import VariablesView from "@/components/VariablesView.vue";
import SequenceView from "@/components/SequenceView.vue";
import TableDesigner from "@/components/TableDesigner.vue";
import TransferDialog from "@/components/TransferDialog.vue";
import type { DbObject, ObjectKind, OpenedSession } from "@/lib/api";
import { newScript } from "@/lib/templates";
import { objectIcon } from "@/lib/objects";
import { cn } from "@/lib/utils";

const props = defineProps<{ session: OpenedSession }>();
const { t } = useI18n();

/** An object tab, an SQL editor tab (obj = null), a "new table / sequence" form (create), or a server screen (admin). */
interface Tab {
  id: string;
  schema: string | null;
  obj: DbObject | null;
  n?: number;
  create?: "table" | "sequence";
  /** Editor tabs: script to start with. */
  sql?: string;
  admin?: Admin;
}
const tabs = ref<Tab[]>([]);
/** null = server info tab. */
const active = ref<string | null>(null);

function open(schema: string, obj: DbObject) {
  const id = `${schema}.${obj.kind}.${obj.name}`;
  if (!tabs.value.some((x) => x.id === id)) tabs.value.push({ id, schema, obj });
  active.value = id;
}

const tree = ref<InstanceType<typeof ObjectTree> | null>(null);
const transfer = ref<InstanceType<typeof TransferDialog> | null>(null);

/** "New…" from the tree: table and sequence have a form, the other kinds start from a template in the editor. */
function create(schema: string, kind: ObjectKind) {
  if (kind === "table" || kind === "sequence") {
    const id = crypto.randomUUID();
    tabs.value.push({ id, schema, obj: null, create: kind });
    active.value = id;
  } else newQuery(schema, newScript(kind));
}

/** The form becomes the new object's tab. */
function created(id: string, schema: string, kind: "table" | "sequence", name: string) {
  close(id);
  open(schema, { name, kind, versioned: false });
  tree.value?.load();
}

function renamed(x: Tab, name: string) {
  x.obj = { ...x.obj!, name };
  x.id = `${x.schema}.${x.obj.kind}.${name}`;
  active.value = x.id;
  tree.value?.load();
}

function dropped(id: string) {
  close(id);
  tree.value?.load();
}

/** Server screens, one tab each; plugins (ALL_PLUGINS) is MariaDB-only. */
type Admin = "processes" | "variables" | "status" | "plugins";
const adminViews = { processes: { icon: Activity, view: ProcessesView }, variables: { icon: SlidersHorizontal, view: VariablesView }, status: { icon: Gauge, view: StatusView }, plugins: { icon: Puzzle, view: PluginsView } };
const adminList = (Object.keys(adminViews) as Admin[]).filter((a) => a !== "plugins" || props.session.info.isMariadb);

function openAdmin(a: Admin) {
  if (!tabs.value.some((x) => x.id === a)) tabs.value.push({ id: a, schema: null, obj: null, admin: a });
  active.value = a;
}

let queries = 0;
// Start on the schema of the tab in front, else the profile's default database.
function newQuery(schema = tabs.value.find((x) => x.id === active.value)?.schema ?? props.session.profile.database, sql?: string) {
  const id = crypto.randomUUID();
  tabs.value.push({ id, schema, obj: null, n: ++queries, sql });
  active.value = id;
}

function close(id: string) {
  const i = tabs.value.findIndex((x) => x.id === id);
  tabs.value.splice(i, 1);
  if (active.value === id) active.value = tabs.value[Math.min(i, tabs.value.length - 1)]?.id ?? null;
}

const tab = "flex h-full shrink-0 items-center gap-1.5 border-r px-3 hover:bg-secondary";
</script>

<template>
  <div class="flex h-full flex-col">
    <div v-if="!session.info.isMariadb" role="alert" class="flex items-center gap-2 border-b bg-warning/10 px-3 py-1.5 text-warning">
      <TriangleAlert class="size-4 shrink-0" />
      {{ t("session.notMariadb") }}
    </div>
    <div class="flex min-h-0 flex-1">
      <aside class="w-64 shrink-0 border-r bg-sidebar">
        <ObjectTree
          ref="tree"
          :session-id="session.sessionId"
          :selected="active"
          :mariadb="session.info.isMariadb"
          @open="open"
          @create="create"
          @transfer="(schema, mode) => transfer?.open({ mode, schema, table: null })"
        />
      </aside>
      <main class="flex min-w-0 flex-1 flex-col">
        <nav class="flex h-8 shrink-0 items-stretch overflow-x-auto border-b bg-sidebar" role="tablist">
          <button role="tab" :aria-selected="active === null" :class="cn(tab, active === null && 'bg-background')" @click="active = null">
            <Info class="size-3.5" /> {{ t("session.server") }}
          </button>
          <div
            v-for="x in tabs"
            :key="x.id"
            role="tab"
            :aria-selected="active === x.id"
            :title="x.obj ? `${x.schema}.${x.obj.name}` : undefined"
            :class="cn(tab, 'cursor-default pr-1', active === x.id && 'bg-background')"
            @click="active = x.id"
            @auxclick.middle="close(x.id)"
          >
            <component :is="x.obj ? objectIcon(x.obj) : x.create === 'table' ? TableProperties : x.create ? ListPlus : x.admin ? adminViews[x.admin].icon : FileCode" class="size-3.5 text-muted-foreground" />
            <span class="max-w-48 truncate">{{ x.obj ? x.obj.name : x.create ? t(`newObject.${x.create}`) : x.admin ? t(`${x.admin}.tab`) : t("editor.tab", { n: x.n }) }}</span>
            <button :title="t('object.close')" class="flex size-5 items-center justify-center rounded-sm text-muted-foreground hover:bg-secondary hover:text-foreground" @click.stop="close(x.id)">
              <X class="size-3" /><span class="sr-only">{{ t("object.close") }}</span>
            </button>
          </div>
          <button :title="t('editor.new')" :class="cn(tab, 'border-r-0 text-muted-foreground')" @click="newQuery()">
            <Plus class="size-3.5" /><span class="sr-only">{{ t("editor.new") }}</span>
          </button>
        </nav>

        <!-- v-show keeps each tab alive, so unsaved grid edits survive switching tabs. -->
        <template v-for="x in tabs" :key="x.id">
          <ObjectView
            v-if="x.obj"
            v-show="active === x.id"
            :session-id="session.sessionId"
            :schema="x.schema!"
            :obj="x.obj"
            :mariadb="session.info.isMariadb"
            class="min-h-0 flex-1"
            @renamed="(n) => renamed(x, n)"
            @dropped="dropped(x.id)"
            @edit-sql="(sql) => newQuery(x.schema, sql)"
            @tree-changed="tree?.load()"
          />
          <SequenceView v-else-if="x.create === 'sequence'" v-show="active === x.id" :session-id="session.sessionId" :schema="x.schema!" name="" create class="min-h-0 flex-1" @created="(n) => created(x.id, x.schema!, 'sequence', n)" />
          <TableDesigner v-else-if="x.create === 'table'" v-show="active === x.id" :session-id="session.sessionId" :schema="x.schema!" :table="null" class="min-h-0 flex-1" @saved="(n) => created(x.id, x.schema!, 'table', n)" />
          <component :is="adminViews[x.admin].view" v-else-if="x.admin" v-show="active === x.id" :session-id="session.sessionId" :active="active === x.id" class="min-h-0 flex-1" />
          <SqlEditor v-else v-show="active === x.id" :session-id="session.sessionId" :profile-id="session.profile.id" :editor-id="x.id" :schema="x.schema" :initial-sql="x.sql" class="min-h-0 flex-1" />
        </template>

        <div v-if="active === null" class="overflow-auto p-5">
          <h2 class="text-sm font-semibold">{{ session.profile.name }}</h2>
          <dl class="mt-3 grid grid-cols-[9rem_1fr] gap-x-3 gap-y-1.5">
            <dt class="text-muted-foreground">{{ t("session.server") }}</dt>
            <dd class="font-mono">{{ session.info.version }} <span class="text-muted-foreground">({{ session.info.versionComment }})</span></dd>
            <dt class="text-muted-foreground">{{ t("session.user") }}</dt>
            <dd class="font-mono">{{ session.info.currentUser }}</dd>
            <dt class="text-muted-foreground">{{ t("session.connectionId") }}</dt>
            <dd class="font-mono">{{ session.info.connectionId }}</dd>
            <dt class="text-muted-foreground">{{ t("session.security") }}</dt>
            <dd class="flex items-center gap-1.5">
              <component :is="session.info.tlsCipher ? Lock : LockOpen" :class="session.info.tlsCipher ? 'size-3.5 text-success' : 'size-3.5 text-warning'" />
              <span class="font-mono">{{ session.info.tlsCipher ? t("session.tlsOn", { cipher: session.info.tlsCipher }) : t("session.tlsOff") }}</span>
              <span v-if="session.info.viaSsh" class="text-muted-foreground">· {{ t("session.viaSsh") }}</span>
            </dd>
          </dl>
          <p class="mt-6 text-muted-foreground">{{ t("session.pickObject") }}</p>
          <div class="mt-3 flex flex-wrap gap-2">
            <button class="flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary" @click="newQuery()">
              <FileCode class="size-3.5" />{{ t("editor.new") }}
            </button>
            <button v-for="a in adminList" :key="a" class="flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary" @click="openAdmin(a)">
              <component :is="adminViews[a].icon" class="size-3.5" />{{ t(`${a}.tab`) }}
            </button>
          </div>
        </div>
      </main>
    </div>
    <TransferDialog ref="transfer" :session-id="session.sessionId" @imported="tree?.load()" />
  </div>
</template>
