import { ref, watch, type Ref } from "vue";

export interface HistoryEntry {
  sql: string;
  at: string;
  schema: string | null;
}
export interface Favorite {
  name: string;
  sql: string;
}

const MAX_HISTORY = 200;

// Query history and favorites per connection profile, kept in the webview's local storage
// (queries only, never secrets). Shared by every editor tab of the same profile.
const cache = new Map<string, { history: Ref<HistoryEntry[]>; favorites: Ref<Favorite[]> }>();

function stored<T>(key: string): Ref<T[]> {
  let initial: T[] = [];
  try {
    initial = JSON.parse(localStorage.getItem(key) ?? "[]");
  } catch {
    // corrupt entry: start empty
  }
  const r = ref(initial) as Ref<T[]>;
  watch(r, (v) => localStorage.setItem(key, JSON.stringify(v)), { deep: true });
  return r;
}

export function useQueryLibrary(profileId: string) {
  let lib = cache.get(profileId);
  if (!lib) {
    lib = { history: stored(`otaria.history.${profileId}`), favorites: stored(`otaria.favorites.${profileId}`) };
    cache.set(profileId, lib);
  }
  const { history, favorites } = lib;

  function remember(sql: string, schema: string | null) {
    if (history.value[0]?.sql === sql) history.value.shift();
    history.value.unshift({ sql, at: new Date().toISOString(), schema });
    history.value.splice(MAX_HISTORY);
  }

  function saveFavorite(name: string, sql: string) {
    favorites.value = [...favorites.value.filter((f) => f.name !== name), { name, sql }].sort((a, b) => a.name.localeCompare(b.name));
  }

  function removeFavorite(name: string) {
    favorites.value = favorites.value.filter((f) => f.name !== name);
  }

  return { history, favorites, remember, saveFavorite, removeFavorite };
}
