<script setup lang="ts">
import { onMounted, ref, shallowRef, watch } from "vue";
import { useI18n } from "vue-i18n";
import { isTauri } from "@tauri-apps/api/core";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { Download, RefreshCw } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError } from "@/lib/api";
import { cn } from "@/lib/utils";

/** Header button + dialog: check GitHub releases for a signed update, install it, restart. */
const { t } = useI18n();
const AUTO_KEY = "otaria.autoUpdate";

const dialog = ref<HTMLDialogElement | null>(null);
const auto = ref(localStorage.getItem(AUTO_KEY) !== "off");
const version = ref("");
// shallowRef: Update keeps #private fields, which a deep reactive Proxy breaks (TypeError on downloadAndInstall).
const update = shallowRef<Update | null>(null);
const state = ref<"idle" | "checking" | "latest" | "downloading">("idle");
const received = ref(0);
const total = ref(0);
const error = ref<AppError | null>(null);

watch(auto, (on) => localStorage.setItem(AUTO_KEY, on ? "on" : "off"));

/** The startup check is silent: offline or a GitHub hiccup must not greet the user with an error. */
async function run(silent: boolean) {
  state.value = "checking";
  error.value = null;
  try {
    update.value = await check();
    state.value = update.value ? "idle" : "latest";
  } catch (e) {
    state.value = "idle";
    if (!silent) error.value = toAppError(e);
  }
}

async function install() {
  const u = update.value!;
  state.value = "downloading";
  received.value = 0;
  total.value = 0;
  try {
    await u.downloadAndInstall((ev) => {
      if (ev.event === "Started") total.value = ev.data.contentLength ?? 0;
      else if (ev.event === "Progress") received.value += ev.data.chunkLength;
    });
    await api.restartApp();
  } catch (e) {
    state.value = "idle";
    error.value = toAppError(e);
  }
}

function open() {
  dialog.value?.showModal();
  if (!update.value && state.value !== "checking") run(false);
}

onMounted(async () => {
  if (!isTauri()) return;
  version.value = (await api.appInfo()).version;
  if (auto.value) run(true);
});

const btn = "flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary disabled:opacity-50";
</script>

<template>
  <button
    :title="t('update.title')"
    :class="cn('flex h-7 items-center gap-1.5 rounded-sm px-1.5 text-muted-foreground hover:bg-secondary hover:text-foreground', update && 'bg-primary px-2 font-semibold text-primary-foreground hover:bg-primary hover:text-primary-foreground')"
    @click="open"
  >
    <component :is="update ? Download : RefreshCw" class="size-3.5" />
    <span v-if="update">{{ t("update.available", { version: update.version }) }}</span>
    <span v-else class="sr-only">{{ t("update.title") }}</span>
  </button>

  <dialog ref="dialog" class="m-auto w-[min(30rem,94vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
    <div class="border-b px-4 py-2 font-semibold">{{ t("update.title") }}</div>
    <div class="flex flex-col gap-3 p-4">
      <p class="text-muted-foreground">{{ t("update.current", { version }) }}</p>
      <p v-if="state === 'checking'" role="status">{{ t("update.checking") }}</p>
      <p v-else-if="state === 'latest'" role="status" class="text-success">{{ t("update.latest") }}</p>
      <template v-if="update">
        <p class="font-semibold">{{ t("update.available", { version: update.version }) }}</p>
        <pre v-if="update.body" class="max-h-48 overflow-auto whitespace-pre-wrap rounded-sm border bg-card p-2 text-xs">{{ update.body }}</pre>
        <p class="text-muted-foreground">{{ t("update.restartWarning") }}</p>
      </template>
      <div v-if="state === 'downloading'" role="status" class="flex flex-col gap-1">
        <span>{{ t("update.downloading") }}<template v-if="total"> {{ Math.round((received / total) * 100) }} %</template></span>
        <progress :value="received" :max="total || undefined" class="w-full accent-primary" />
      </div>
      <ErrorLine v-if="error" :error="error" />
      <label class="flex items-center gap-1.5"><input v-model="auto" type="checkbox" class="size-3.5" />{{ t("update.auto") }}</label>
    </div>
    <div class="flex justify-end gap-2 border-t px-4 py-3">
      <button :class="btn" :disabled="state === 'downloading'" @click="dialog?.close()">{{ t("update.close") }}</button>
      <button :class="btn" :disabled="state === 'checking' || state === 'downloading'" @click="run(false)">{{ t("update.check") }}</button>
      <button
        v-if="update"
        :disabled="state === 'downloading'"
        class="flex h-7 items-center gap-1.5 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50"
        @click="install"
      >
        <Download class="size-3.5" />{{ t("update.install") }}
      </button>
    </div>
  </dialog>
</template>
