<script setup lang="ts">
import { onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { Plus, Server } from "lucide-vue-next";
import ConnectionForm from "@/components/ConnectionForm.vue";
import { api, newProfile, type Profile } from "@/lib/api";
import { tagClass } from "@/lib/tagColors";
import { useSessions } from "@/composables/useSessions";
import { cn } from "@/lib/utils";

const { t } = useI18n();
const { profiles, reloadProfiles, addSession } = useSessions();
const selected = ref<Profile>(newProfile());
const listError = ref("");

onMounted(async () => {
  try {
    await reloadProfiles();
    if (profiles.value[0]) selected.value = profiles.value[0];
  } catch (e) {
    listError.value = String((e as { message?: string }).message ?? e);
  }
});

/** Double-click: connect straight away with the saved password (errors show in the form). */
async function quickConnect(p: Profile) {
  selected.value = p;
  if (p.savePassword) {
    try {
      addSession(await api.connect(p.id));
    } catch {
      /* the form's Connect button shows the detailed error */
    }
  }
}
</script>

<template>
  <div class="flex h-full min-h-0">
    <aside class="flex w-60 shrink-0 flex-col border-r bg-sidebar">
      <div class="flex h-9 items-center justify-between border-b px-3">
        <span class="font-semibold">{{ t("connection.saved") }}</span>
        <button :title="t('connection.new')" class="flex size-6 items-center justify-center rounded-sm hover:bg-secondary" @click="selected = newProfile()">
          <Plus class="size-4" /><span class="sr-only">{{ t("connection.new") }}</span>
        </button>
      </div>
      <ul class="min-h-0 flex-1 overflow-auto py-1">
        <li v-for="p in profiles" :key="p.id">
          <button
            :class="cn('flex w-full items-center gap-2 px-3 py-1 text-left hover:bg-secondary', selected.id === p.id && 'bg-selection')"
            @click="selected = p"
            @dblclick="quickConnect(p)"
          >
            <span :class="cn('size-2 shrink-0 rounded-full', tagClass(p.color))" />
            <span class="min-w-0 flex-1 truncate">{{ p.name }}</span>
            <span class="truncate font-mono text-xs text-muted-foreground">{{ p.ssh ? "ssh" : "" }}</span>
          </button>
        </li>
        <li v-if="!profiles.length" class="flex flex-col items-center gap-2 px-4 py-8 text-center text-muted-foreground">
          <Server class="size-6" />
          {{ t("connection.empty") }}
        </li>
      </ul>
      <p v-if="listError" class="border-t p-2 font-mono text-xs text-destructive">{{ listError }}</p>
    </aside>
    <div class="min-w-0 flex-1 overflow-auto">
      <ConnectionForm :profile="selected" @saved="(p) => (selected = p)" @deleted="selected = newProfile()" />
    </div>
  </div>
</template>
