import { createI18n } from "vue-i18n";
import en from "./locales/en.json";
import fr from "./locales/fr.json";
import es from "./locales/es.json";
import it from "./locales/it.json";
import de from "./locales/de.json";

// English is the source language: every key exists in en.json first.
export const LOCALES = { en: "English", fr: "Français", es: "Español", it: "Italiano", de: "Deutsch" } as const;
export type Locale = keyof typeof LOCALES;

const STORAGE_KEY = "otaria.locale";

function initialLocale(): Locale {
  const saved = localStorage.getItem(STORAGE_KEY);
  if (saved && saved in LOCALES) return saved as Locale;
  // ponytail: navigator.language mirrors the OS language in the webview; add tauri-plugin-os if it ever doesn't
  const system = navigator.language.slice(0, 2);
  return system in LOCALES ? (system as Locale) : "en";
}

export const i18n = createI18n({
  legacy: false,
  locale: initialLocale(),
  fallbackLocale: "en",
  messages: { en, fr, es, it, de },
});

export function setLocale(locale: Locale) {
  i18n.global.locale.value = locale;
  localStorage.setItem(STORAGE_KEY, locale);
  document.documentElement.lang = locale;
}

document.documentElement.lang = i18n.global.locale.value;
