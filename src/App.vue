<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { Monitor, Moon, Plug, Sun, X } from "lucide-vue-next";
import mark from "@/assets/mark.svg";
import ConnectionManager from "@/components/ConnectionManager.vue";
import SessionView from "@/components/SessionView.vue";
import { LOCALES, setLocale, type Locale } from "@/i18n";
import { useTheme, type ThemePreference } from "@/composables/useTheme";
import { useSessions } from "@/composables/useSessions";
import { tagClass } from "@/lib/tagColors";
import { cn } from "@/lib/utils";

const { t, locale } = useI18n();
const { preference } = useTheme();
const { sessions, activeTab, closeSession } = useSessions();

const themes: { value: ThemePreference; icon: typeof Sun }[] = [
  { value: "system", icon: Monitor },
  { value: "light", icon: Sun },
  { value: "dark", icon: Moon },
];
const tab = "flex h-full items-center gap-1.5 border-r px-3 hover:bg-secondary";
</script>

<template>
  <div class="flex h-screen flex-col">
    <header class="flex h-9 shrink-0 items-stretch border-b bg-sidebar">
      <div class="flex items-center gap-2 border-r px-3">
        <img :src="mark" alt="Otaria" class="size-5" />
      </div>
      <nav class="flex min-w-0 flex-1 items-stretch overflow-x-auto" role="tablist">
        <button role="tab" :aria-selected="activeTab === 'manager'" :class="cn(tab, activeTab === 'manager' && 'bg-background')" @click="activeTab = 'manager'">
          <Plug class="size-3.5" /> {{ t("connection.title") }}
        </button>
        <div
          v-for="s in sessions"
          :key="s.sessionId"
          role="tab"
          :aria-selected="activeTab === s.sessionId"
          :class="cn(tab, 'cursor-default pr-1', activeTab === s.sessionId && 'bg-background')"
          @click="activeTab = s.sessionId"
        >
          <span :class="cn('size-2 rounded-full', tagClass(s.profile.color))" />
          <span class="max-w-40 truncate">{{ s.profile.name }}</span>
          <button :title="t('session.disconnect')" class="flex size-5 items-center justify-center rounded-sm text-muted-foreground hover:bg-secondary hover:text-foreground" @click.stop="closeSession(s.sessionId)">
            <X class="size-3" /><span class="sr-only">{{ t("session.disconnect") }}</span>
          </button>
        </div>
      </nav>

      <div class="flex items-center gap-3 px-3">
        <div role="radiogroup" :aria-label="t('settings.theme')" class="flex rounded-md border p-0.5">
          <button
            v-for="th in themes"
            :key="th.value"
            role="radio"
            :aria-checked="preference === th.value"
            :title="t(`settings.${th.value}`)"
            :class="cn('flex size-6 items-center justify-center rounded-sm text-muted-foreground hover:text-foreground', preference === th.value && 'bg-selection text-foreground')"
            @click="preference = th.value"
          >
            <component :is="th.icon" class="size-3.5" />
            <span class="sr-only">{{ t(`settings.${th.value}`) }}</span>
          </button>
        </div>
        <label>
          <span class="sr-only">{{ t("settings.language") }}</span>
          <select :value="locale" class="h-7 rounded-sm border bg-background px-1.5" @change="setLocale(($event.target as HTMLSelectElement).value as Locale)">
            <option v-for="(name, code) in LOCALES" :key="code" :value="code">{{ name }}</option>
          </select>
        </label>
      </div>
    </header>

    <main class="min-h-0 flex-1">
      <ConnectionManager v-show="activeTab === 'manager'" />
      <SessionView v-for="s in sessions" v-show="activeTab === s.sessionId" :key="s.sessionId" :session="s" />
    </main>
  </div>
</template>
