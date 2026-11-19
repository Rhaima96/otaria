<script setup lang="ts">
import { computed, reactive, ref } from "vue";
import { useI18n } from "vue-i18n";
import { Network, Pencil, Play, Plus, Share2, SkipForward, Square } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type ChangeMaster, type ReplicationAction, type ReplicationChannel, type ReplicationState } from "@/lib/api";
import { INTERVALS, usePolling } from "@/lib/poll";
import { cn } from "@/lib/utils";

/** Replication channels (multi-source) and Galera cluster of the server, read-only, refreshed. */
const props = defineProps<{ sessionId: string; active?: boolean }>();
const { t, locale } = useI18n();

const st = ref<ReplicationState | null>(null);
const error = ref<AppError | null>(null);

const { every, tick } = usePolling(
  async () => {
    try {
      st.value = await api.replicationState(props.sessionId);
      error.value = null;
    } catch (e) {
      error.value = toAppError(e);
    }
  },
  () => props.active !== false,
  ref(5),
);

const runClass = (v: string) => (v === "Yes" ? "text-success" : v === "No" ? "text-destructive" : "text-warning");
const healthy = (c: ReplicationChannel) => c.ioRunning === "Yes" && c.sqlRunning === "Yes";
const lag = (c: ReplicationChannel) => (c.secondsBehind === null ? "—" : t("replication.seconds", { n: c.secondsBehind.toLocaleString(locale.value) }));

const g = computed(() => st.value?.galera ?? null);
const gs = (k: string) => g.value?.status[k] ?? "—";
// Fraction of time replication was paused by flow control since the last FLUSH STATUS.
const paused = computed(() => {
  const v = Number(g.value?.status.flow_control_paused);
  return Number.isFinite(v) ? new Intl.NumberFormat(locale.value, { style: "percent", maximumFractionDigits: 1 }).format(v) : "—";
});
const galeraOk = computed(() => gs("cluster_status") === "Primary" && gs("local_state_comment") === "Synced" && gs("ready") === "ON");

// Channel actions: preview of the exact SQL, then run (MariaDB rebuilds it from the same inputs).
const actDialog = ref<HTMLDialogElement | null>(null);
const pending = ref<{ name: string; action: ReplicationAction; sql: string[] } | null>(null);
const skipCount = ref(1);
const actError = ref<AppError | null>(null);
const busy = ref(false);
async function ask(name: string, action: ReplicationAction) {
  actError.value = null;
  try {
    pending.value = { name, action, sql: await api.replicationSql(name, action) };
    if (!actDialog.value?.open) actDialog.value?.showModal();
  } catch (e) {
    // Form fields being typed: the dialog stays open with the error under them.
    if (actDialog.value?.open) {
      pending.value = { name, action, sql: [] };
      actError.value = toAppError(e);
    } else error.value = toAppError(e);
  }
}

// CHANGE MASTER TO form: new channel (name editable) or an existing one (prefilled, password left unchanged).
// The password lives only in this form while the dialog is open: cleared on close, never shown in the preview.
// Typing empties the preview (Apply off) until the field is left: what runs is always what was shown.
const form = reactive<ChangeMaster & { name: string; isNew: boolean }>({ name: "", isNew: true, host: "", port: 3306, user: "", password: "", gtid: "slave_pos", file: "", pos: 4 });
const change = () => {
  const { name, isNew: _, ...cm } = form;
  if (cm.host) return ask(name, { kind: "change", ...cm });
  pending.value = { name, action: { kind: "change", ...cm }, sql: [] }; // nothing to preview before a host
  actError.value = null;
};
function openChange(c: ReplicationChannel | null) {
  const gtid = c?.usingGtid === "Current_Pos" ? "current_pos" : c?.usingGtid === "No" ? "no" : "slave_pos";
  Object.assign(form, { name: c?.name ?? "", isNew: !c, host: c?.masterHost ?? "", port: c?.masterPort || 3306, user: c?.masterUser ?? "", password: "", gtid, file: "", pos: 4 });
  change();
  actDialog.value?.showModal();
}
function closed() {
  form.password = "";
  pending.value = null;
}
async function act() {
  const p = pending.value!;
  busy.value = true;
  actError.value = null;
  try {
    await api.replicationApply(props.sessionId, p.name, p.action);
    actDialog.value?.close();
    await tick();
  } catch (e) {
    actError.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}
const btn = "flex h-6 items-center gap-1 rounded-sm px-1.5 text-muted-foreground hover:bg-secondary hover:text-foreground";

const h = "mb-2 flex items-center gap-1.5 font-semibold";
const card = "rounded-md border p-3";
const cell = "border-b px-2 py-1 text-left align-top";
const label = "text-muted-foreground";
const field = "h-7 rounded-sm border bg-background px-2 disabled:opacity-60";
</script>

<template>
  <div class="relative flex min-h-0 flex-col overflow-auto">
    <div class="flex h-9 shrink-0 items-center gap-2 border-b px-3">
      <label class="ml-auto flex items-center gap-1.5 text-muted-foreground">
        {{ t("admin.refreshEvery") }}
        <select v-model.number="every" class="h-6 rounded-sm border bg-background px-1">
          <option v-for="n in INTERVALS" :key="n" :value="n">{{ n ? t("admin.seconds", { n }) : t("admin.paused") }}</option>
        </select>
      </label>
    </div>

    <ErrorLine v-if="error" :error="error" class="m-3" />

    <div v-if="st" class="flex flex-col gap-4 p-4">
      <!-- Replication channels -->
      <section :class="card">
        <h3 :class="h">
          <Share2 class="size-4 text-primary" />{{ t("replication.channels") }}
          <button :class="cn(btn, 'ml-auto font-normal')" :title="t('replication.changeHint')" @click="openChange(null)"><Plus class="size-3.5" />{{ t("replication.newChannel") }}</button>
        </h3>
        <p v-if="!st.channels.length" class="text-muted-foreground">{{ t("replication.noChannels") }}</p>
        <table v-else class="w-full border-collapse">
          <thead :class="label">
            <tr>
              <th :class="cell">{{ t("replication.channel") }}</th>
              <th :class="cell">{{ t("replication.source") }}</th>
              <th :class="cell">{{ t("replication.io") }}</th>
              <th :class="cell">{{ t("replication.sql") }}</th>
              <th :class="cell">{{ t("replication.lag") }}</th>
              <th :class="cell">{{ t("replication.position") }}</th>
              <th :class="cell"><span class="sr-only">{{ t("processes.actions") }}</span></th>
            </tr>
          </thead>
          <tbody>
            <template v-for="c in st.channels" :key="c.name">
              <tr>
                <td :class="cn(cell, 'font-mono', !c.name && 'italic text-muted-foreground')">{{ c.name || t("replication.default") }}</td>
                <td :class="cn(cell, 'font-mono')">{{ c.masterHost }}:{{ c.masterPort }}</td>
                <td :class="cn(cell, runClass(c.ioRunning))" :title="c.ioState || undefined">{{ c.ioRunning }}</td>
                <td :class="cn(cell, runClass(c.sqlRunning))" :title="c.sqlState || undefined">{{ c.sqlRunning }}</td>
                <td :class="cn(cell, healthy(c) && c.secondsBehind ? 'text-warning' : '')">{{ lag(c) }}</td>
                <td :class="cn(cell, 'font-mono text-xs')">
                  <template v-if="c.usingGtid !== 'No'">{{ t("replication.gtid") }} {{ c.gtidIoPos || "—" }}</template>
                  <template v-else>{{ c.masterLogFile || "—" }} · {{ c.readPos }} / {{ c.execPos }}</template>
                </td>
                <td :class="cn(cell, 'whitespace-nowrap')">
                  <div class="flex gap-0.5">
                    <button v-if="!healthy(c)" :class="btn" :title="t('replication.start')" @click="ask(c.name, { kind: 'start' })">
                      <Play class="size-3.5" /><span class="sr-only">{{ t("replication.start") }}</span>
                    </button>
                    <button v-if="c.ioRunning !== 'No' || c.sqlRunning !== 'No'" :class="btn" :title="t('replication.stop')" @click="ask(c.name, { kind: 'stop' })">
                      <Square class="size-3.5" /><span class="sr-only">{{ t("replication.stop") }}</span>
                    </button>
                    <button v-if="c.ioRunning === 'No' && c.sqlRunning === 'No'" :class="btn" :title="t('replication.change')" @click="openChange(c)">
                      <Pencil class="size-3.5" /><span class="sr-only">{{ t("replication.change") }}</span>
                    </button>
                    <button v-if="c.sqlRunning === 'No' && c.lastSqlError" :class="btn" :title="t('replication.skipHint')" @click="ask(c.name, { kind: 'skip', count: skipCount })">
                      <SkipForward class="size-3.5" />{{ t("replication.skip") }}
                    </button>
                  </div>
                </td>
              </tr>
              <tr v-if="c.lastIoError || c.lastSqlError">
                <td colspan="7" :class="cn(cell, 'font-mono text-xs text-destructive')">
                  <p v-if="c.lastIoError">{{ t("replication.io") }} · {{ c.lastIoError }}</p>
                  <p v-if="c.lastSqlError">{{ t("replication.sql") }} · {{ c.lastSqlError }}</p>
                </td>
              </tr>
            </template>
          </tbody>
        </table>
      </section>

      <dialog ref="actDialog" class="m-auto w-[min(36rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40" @close="closed">
        <div class="border-b px-4 py-2 font-semibold">{{ t("grid.previewTitle") }}</div>
        <div v-if="pending?.action.kind === 'change'" class="grid grid-cols-[max-content_1fr] items-center gap-x-3 gap-y-1.5 px-4 pt-3"
          @input="pending!.sql = []"
          @change="change"
        >
          <label :class="label" for="cm-name">{{ t("replication.channel") }}</label>
          <input id="cm-name" v-model.trim="form.name" :disabled="!form.isNew" :placeholder="t('replication.default')" :class="field" />
          <label :class="label" for="cm-host">{{ t("replication.host") }}</label>
          <div class="flex gap-2">
            <input id="cm-host" v-model.trim="form.host" :class="cn(field, 'flex-1')" />
            <input v-model.number="form.port" type="number" min="1" max="65535" :aria-label="t('replication.port')" :class="cn(field, 'w-24')" />
          </div>
          <label :class="label" for="cm-user">{{ t("replication.user") }}</label>
          <input id="cm-user" v-model="form.user" autocomplete="off" :class="field" />
          <label :class="label" for="cm-pw">{{ t("replication.password") }}</label>
          <input id="cm-pw" v-model="form.password" type="password" autocomplete="new-password" :placeholder="form.isNew ? '' : t('replication.passwordKeep')" :class="field" />
          <label :class="label" for="cm-gtid">{{ t("replication.startAt") }}</label>
          <select id="cm-gtid" v-model="form.gtid" :class="field">
            <option value="slave_pos">{{ t("replication.slavePos") }}</option>
            <option value="current_pos">{{ t("replication.currentPos") }}</option>
            <option value="no">{{ t("replication.filePos") }}</option>
          </select>
          <template v-if="form.gtid === 'no'">
            <label :class="label" for="cm-file">{{ t("replication.file") }}</label>
            <div class="flex gap-2">
              <input id="cm-file" v-model.trim="form.file" :placeholder="t('replication.fileKeep')" :class="cn(field, 'flex-1 font-mono')" />
              <input v-model.number="form.pos" type="number" min="4" :aria-label="t('binlog.pos')" :class="cn(field, 'w-28 font-mono')" />
            </div>
          </template>
        </div>
        <label v-if="pending?.action.kind === 'skip'" class="flex items-center gap-2 px-4 pt-3">
          {{ t("replication.skipCount") }}
          <input
            v-model.number="skipCount"
            type="number"
            min="1"
            max="1000"
            class="h-7 w-20 rounded-sm border bg-background px-2"
            @change="ask(pending!.name, { kind: 'skip', count: skipCount })"
          />
        </label>
        <pre v-if="pending?.sql.length" class="max-h-[40vh] overflow-auto p-4 font-mono text-xs whitespace-pre-wrap select-text">{{ pending.sql.join(";\n") + ";" }}</pre>
        <p v-if="pending?.action.kind === 'skip'" class="px-4 text-warning">{{ t("replication.skipWarning") }}</p>
        <p v-if="pending?.action.kind === 'change'" class="px-4 text-muted-foreground">{{ t("replication.changeNote") }}</p>
        <ErrorLine v-if="actError" :error="actError" class="mx-4 mt-2" />
        <div class="flex justify-end gap-2 px-4 py-3">
          <button class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="actDialog?.close()">{{ t("grid.cancel") }}</button>
          <button :disabled="busy || !pending?.sql.length" class="h-7 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50" @click="act">{{ t("grid.apply") }}</button>
        </div>
      </dialog>

      <!-- Galera -->
      <section :class="card">
        <h3 :class="h"><Network class="size-4 text-primary" />{{ t("replication.galera") }}</h3>
        <p v-if="!g" class="text-muted-foreground">{{ t("replication.noGalera") }}</p>
        <template v-else>
          <dl class="mb-3 grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 sm:grid-cols-[max-content_1fr_max-content_1fr]">
            <dt :class="label">{{ t("replication.cluster") }}</dt>
            <dd class="font-mono">{{ g.clusterName }}</dd>
            <dt :class="label">{{ t("replication.node") }}</dt>
            <dd class="font-mono">{{ g.nodeName }}</dd>
            <dt :class="label">{{ t("replication.clusterStatus") }}</dt>
            <dd :class="galeraOk ? 'text-success' : 'text-destructive'">{{ gs("cluster_status") }} · {{ gs("local_state_comment") }}</dd>
            <dt :class="label">{{ t("replication.size") }}</dt>
            <dd>{{ gs("cluster_size") }}</dd>
            <dt :class="label">{{ t("replication.ready") }}</dt>
            <dd :class="gs('ready') === 'ON' ? 'text-success' : 'text-destructive'">{{ gs("ready") }}</dd>
            <dt :class="label" :title="t('replication.flowPausedHint')">{{ t("replication.flowPaused") }}</dt>
            <dd :class="Number(g.status.flow_control_paused) > 0.1 ? 'text-warning' : ''">{{ paused }}</dd>
            <dt :class="label">{{ t("replication.flowSent") }}</dt>
            <dd>{{ gs("flow_control_sent") }} / {{ gs("flow_control_recv") }}</dd>
            <dt :class="label" :title="t('replication.queueHint')">{{ t("replication.queues") }}</dt>
            <dd>{{ gs("local_recv_queue_avg") }} / {{ gs("local_send_queue_avg") }}</dd>
            <dt :class="label">{{ t("replication.conflicts") }}</dt>
            <dd>{{ gs("local_cert_failures") }} / {{ gs("local_bf_aborts") }}</dd>
            <dt :class="label">{{ t("replication.provider") }}</dt>
            <dd class="font-mono text-xs">{{ gs("provider_version") }}</dd>
          </dl>
          <table v-if="g.nodes.length" class="w-full border-collapse">
            <thead :class="label">
              <tr>
                <th :class="cell">#</th>
                <th :class="cell">{{ t("replication.node") }}</th>
                <th :class="cell">{{ t("replication.address") }}</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="n in g.nodes" :key="n.index">
                <td :class="cell">{{ n.index }}</td>
                <td :class="cn(cell, 'font-mono', n.name === g.nodeName && 'text-primary')">{{ n.name }}</td>
                <td :class="cn(cell, 'font-mono')">{{ n.address }}</td>
              </tr>
            </tbody>
          </table>
          <template v-else>
            <!-- WSREP_MEMBERSHIP is empty without SUPER: addresses only, from the status. -->
            <ul v-if="g.status.incoming_addresses" class="mb-1 font-mono">
              <li v-for="a in g.status.incoming_addresses.split(',')" :key="a">{{ a }}</li>
            </ul>
            <p class="text-muted-foreground">{{ t("replication.noMembership") }}</p>
          </template>
        </template>
      </section>
    </div>
  </div>
</template>
