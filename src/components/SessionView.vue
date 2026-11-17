<script setup lang="ts">
import { defineAsyncComponent, ref } from "vue";
import { useI18n } from "vue-i18n";
import { FileCode, Info, Lock, LockOpen, Plus, TriangleAlert, X } from "lucide-vue-next";
import ObjectTree from "@/components/ObjectTree.vue";
import ObjectView from "@/components/ObjectView.vue";
// Monaco is heavy: load the editor on first use.
const SqlEditor = defineAsyncComponent(() => import("@/components/SqlEditor.vue"));
import type { DbObject, OpenedSession } from "@/lib/api";
import { objectIcon } from "@/lib/objects";
import { cn } from "@/lib/utils";

const props = defineProps<{ session: OpenedSession }>();
const { t } = useI18n();

/** An object tab, or an SQL editor tab (obj = null). */
interface Tab {
  id: string;
  schema: string | null;
  obj: DbObject | null;
  n?: number;
}
const tabs = ref<Tab[]>([]);
/** null = server info tab. */
const active = ref<string | null>(null);

function open(schema: string, obj: DbObject) {
  const id = `${schema}.${obj.kind}.${obj.name}`;
  if (!tabs.value.some((x) => x.id === id)) tabs.value.push({ id, schema, obj });
  active.value = id;
}

let queries = 0;
function newQuery() {
  // Start on the schema of the tab in front, else the profile's default database.
  const schema = tabs.value.find((x) => x.id === active.value)?.schema ?? props.session.profile.database;
  const id = crypto.randomUUID();
  tabs.value.push({ id, schema, obj: null, n: ++queries });
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
        <ObjectTree :session-id="session.sessionId" :selected="active" @open="open" />
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
            <component :is="x.obj ? objectIcon(x.obj) : FileCode" class="size-3.5 text-muted-foreground" />
            <span class="max-w-48 truncate">{{ x.obj ? x.obj.name : t("editor.tab", { n: x.n }) }}</span>
            <button :title="t('object.close')" class="flex size-5 items-center justify-center rounded-sm text-muted-foreground hover:bg-secondary hover:text-foreground" @click.stop="close(x.id)">
              <X class="size-3" /><span class="sr-only">{{ t("object.close") }}</span>
            </button>
          </div>
          <button :title="t('editor.new')" :class="cn(tab, 'border-r-0 text-muted-foreground')" @click="newQuery">
            <Plus class="size-3.5" /><span class="sr-only">{{ t("editor.new") }}</span>
          </button>
        </nav>

        <!-- v-show keeps each tab alive, so unsaved grid edits survive switching tabs. -->
        <template v-for="x in tabs" :key="x.id">
          <ObjectView v-if="x.obj" v-show="active === x.id" :session-id="session.sessionId" :schema="x.schema!" :obj="x.obj" class="min-h-0 flex-1" />
          <SqlEditor v-else v-show="active === x.id" :session-id="session.sessionId" :profile-id="session.profile.id" :editor-id="x.id" :schema="x.schema" class="min-h-0 flex-1" />
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
          <button class="mt-3 flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary" @click="newQuery">
            <FileCode class="size-3.5" />{{ t("editor.new") }}
          </button>
        </div>
      </main>
    </div>
  </div>
</template>
