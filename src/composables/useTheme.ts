import { ref, watchEffect } from "vue";
import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

export type ThemePreference = "system" | "light" | "dark";

const STORAGE_KEY = "otaria.theme";
const media = window.matchMedia("(prefers-color-scheme: dark)");

const preference = ref<ThemePreference>((localStorage.getItem(STORAGE_KEY) as ThemePreference) || "system");
const systemDark = ref(media.matches);
media.addEventListener("change", (e) => (systemDark.value = e.matches));

watchEffect(() => {
  const dark = preference.value === "dark" || (preference.value === "system" && systemDark.value);
  document.documentElement.classList.toggle("dark", dark);
  localStorage.setItem(STORAGE_KEY, preference.value);
  // Native title bar follows the choice; null = follow the OS. Skipped outside Tauri (plain browser dev).
  if (isTauri()) getCurrentWindow().setTheme(preference.value === "system" ? null : preference.value);
});

/** Light/dark/system theme, persisted and applied to <html class="dark">. */
export function useTheme() {
  return { preference };
}
