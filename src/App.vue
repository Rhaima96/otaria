<script setup lang="ts">
import { onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { Monitor, Moon, Sun } from "lucide-vue-next";
import mark from "@/assets/mark.svg";
import { api, type AppInfo } from "@/lib/api";
import { LOCALES, setLocale, type Locale } from "@/i18n";
import { useTheme, type ThemePreference } from "@/composables/useTheme";
import { cn } from "@/lib/utils";

const { t, locale } = useI18n();
const { preference } = useTheme();
const info = ref<AppInfo | null>(null);
const ipcError = ref(false);

const themes: { value: ThemePreference; icon: typeof Sun }[] = [
  { value: "system", icon: Monitor },
  { value: "light", icon: Sun },
  { value: "dark", icon: Moon },
];

onMounted(async () => {
  try {
    info.value = await api.appInfo();
  } catch {
    ipcError.value = true; // e.g. running in a plain browser via `npm run dev`
  }
});
</script>

<template>
  <div class="flex h-screen flex-col">
    <header class="flex h-10 items-center gap-2 border-b bg-sidebar px-3">
      <img :src="mark" alt="" class="size-5" />
      <span class="font-semibold">Otaria</span>
      <span class="truncate text-muted-foreground">{{ t("app.tagline") }}</span>

      <div class="ml-auto flex items-center gap-3">
        <div role="radiogroup" :aria-label="t('settings.theme')" class="flex rounded-md border p-0.5">
          <button
            v-for="th in themes"
            :key="th.value"
            role="radio"
            :aria-checked="preference === th.value"
            :title="t(`settings.${th.value}`)"
            :class="cn(
              'flex size-6 items-center justify-center rounded-sm text-muted-foreground hover:text-foreground',
              preference === th.value && 'bg-selection text-foreground',
            )"
            @click="preference = th.value"
          >
            <component :is="th.icon" class="size-3.5" />
            <span class="sr-only">{{ t(`settings.${th.value}`) }}</span>
          </button>
        </div>

        <label class="flex items-center gap-1.5 text-muted-foreground">
          <span class="sr-only">{{ t("settings.language") }}</span>
          <select
            :value="locale"
            class="h-7 rounded-sm border bg-background px-1.5 text-foreground"
            @change="setLocale(($event.target as HTMLSelectElement).value as Locale)"
          >
            <option v-for="(name, code) in LOCALES" :key="code" :value="code">{{ name }}</option>
          </select>
        </label>
      </div>
    </header>

    <main class="flex flex-1 flex-col items-center justify-center gap-3 p-8 text-center">
      <img :src="mark" alt="Otaria" class="size-24" />
      <h1 class="text-lg font-semibold">Otaria <span class="font-normal text-muted-foreground">for MariaDB</span></h1>
      <p class="max-w-md">{{ t("app.welcome") }}</p>
      <p v-if="info" class="font-mono text-xs text-success">✓ {{ t("app.version", { version: info.version, os: info.os }) }}</p>
      <p v-else-if="ipcError" class="font-mono text-xs text-warning">⚠ {{ t("errors.ipc") }}</p>
    </main>
  </div>
</template>
