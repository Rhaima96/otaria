<script setup lang="ts">
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import { Network, Share2 } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type ReplicationChannel, type ReplicationState } from "@/lib/api";
import { INTERVALS, usePolling } from "@/lib/poll";
import { cn } from "@/lib/utils";

/** Replication channels (multi-source) and Galera cluster of the server, read-only, refreshed. */
const props = defineProps<{ sessionId: string; active?: boolean }>();
const { t, locale } = useI18n();

const st = ref<ReplicationState | null>(null);
const error = ref<AppError | null>(null);

const { every } = usePolling(
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

const h = "mb-2 flex items-center gap-1.5 font-semibold";
const card = "rounded-md border p-3";
const cell = "border-b px-2 py-1 text-left align-top";
const label = "text-muted-foreground";
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
        <h3 :class="h"><Share2 class="size-4 text-primary" />{{ t("replication.channels") }}</h3>
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
              </tr>
              <tr v-if="c.lastIoError || c.lastSqlError">
                <td colspan="6" :class="cn(cell, 'font-mono text-xs text-destructive')">
                  <p v-if="c.lastIoError">{{ t("replication.io") }} · {{ c.lastIoError }}</p>
                  <p v-if="c.lastSqlError">{{ t("replication.sql") }} · {{ c.lastSqlError }}</p>
                </td>
              </tr>
            </template>
          </tbody>
        </table>
      </section>

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
