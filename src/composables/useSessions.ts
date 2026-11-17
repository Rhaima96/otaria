import { ref } from "vue";
import { api, type OpenedSession, type Profile } from "@/lib/api";

// App-wide connection state: saved profiles, open sessions and the active tab.
const profiles = ref<Profile[]>([]);
const sessions = ref<OpenedSession[]>([]);
/** "manager" or a sessionId. */
const activeTab = ref<string>("manager");

async function reloadProfiles() {
  profiles.value = await api.listProfiles();
}

function addSession(s: OpenedSession) {
  sessions.value.push(s);
  activeTab.value = s.sessionId;
}

async function closeSession(sessionId: string) {
  await api.disconnect(sessionId).catch(() => {}); // already gone server-side: still close the tab
  sessions.value = sessions.value.filter((s) => s.sessionId !== sessionId);
  if (activeTab.value === sessionId) activeTab.value = sessions.value.at(-1)?.sessionId ?? "manager";
}

export function useSessions() {
  return { profiles, sessions, activeTab, reloadProfiles, addSession, closeSession };
}
