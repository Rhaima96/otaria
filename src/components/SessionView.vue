<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { Lock, LockOpen, TriangleAlert } from "lucide-vue-next";
import type { OpenedSession } from "@/lib/api";

defineProps<{ session: OpenedSession }>();
const { t } = useI18n();
</script>

<template>
  <div class="flex h-full flex-col">
    <div v-if="!session.info.isMariadb" role="alert" class="flex items-center gap-2 border-b bg-warning/10 px-3 py-1.5 text-warning">
      <TriangleAlert class="size-4 shrink-0" />
      {{ t("session.notMariadb") }}
    </div>
    <div class="p-5">
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
      <p class="mt-6 text-muted-foreground">{{ t("session.nextPhase") }}</p>
    </div>
  </div>
</template>
