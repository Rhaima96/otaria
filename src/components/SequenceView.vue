<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { Copy, RefreshCw, Save } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { api, toAppError, type AppError, type SeqOpts, type SeqState } from "@/lib/api";

/** `create`: new sequence form (name editable); otherwise state and ALTER of an existing one. */
const props = defineProps<{ sessionId: string; schema: string; name: string; create?: boolean }>();
const emit = defineEmits<{ created: [name: string] }>();
const { t } = useI18n();

const name = ref(props.name);
const state = ref<SeqState | null>(null);
// MariaDB defaults for CREATE SEQUENCE.
const opts = ref<SeqOpts>({ start: "1", min: "1", max: "9223372036854775806", increment: "1", cache: "1000", cycle: false, restart: "" });
const error = ref<AppError | null>(null);
const busy = ref(false);

async function load() {
  if (props.create) return;
  error.value = null;
  try {
    state.value = await api.sequenceState(props.sessionId, props.schema, name.value);
    opts.value = { ...state.value.opts };
  } catch (e) {
    error.value = toAppError(e);
  }
}
onMounted(load);

// CREATE / ALTER: preview the statement, then run it (DDL commits on its own, no transaction).
const dialog = ref<HTMLDialogElement | null>(null);
const preview = ref("");
const applyError = ref<AppError | null>(null);

async function openPreview() {
  error.value = null;
  applyError.value = null;
  try {
    preview.value = await api.sequenceDdl(props.schema, name.value.trim(), opts.value, !!props.create);
    dialog.value?.showModal();
  } catch (e) {
    error.value = toAppError(e);
  }
}

async function apply() {
  busy.value = true;
  applyError.value = null;
  try {
    await api.sequenceApply(props.sessionId, props.schema, name.value.trim(), opts.value, !!props.create);
    dialog.value?.close();
    if (props.create) emit("created", name.value.trim());
    else await load();
  } catch (e) {
    applyError.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

// SETVAL: the server ignores a value behind the current one (RESTART does that).
const nextValue = ref("");
const setvalResult = ref<"ok" | "ignored" | null>(null);
async function setval() {
  error.value = null;
  setvalResult.value = null;
  try {
    const r = await api.sequenceSetval(props.sessionId, props.schema, name.value, nextValue.value);
    setvalResult.value = r === null ? "ignored" : "ok";
    await load();
  } catch (e) {
    error.value = toAppError(e);
  }
}

const quote = (s: string) => "`" + s.replace(/`/g, "``") + "`";
const defaultExpr = computed(() => `DEFAULT NEXTVAL(${quote(props.schema)}.${quote(name.value)})`);
const copied = ref(false);
async function copyDefault() {
  await navigator.clipboard.writeText(defaultExpr.value);
  copied.value = true;
  setTimeout(() => (copied.value = false), 1500);
}

const fields = ["start", "min", "max", "increment", "cache"] as const;
const input = "h-7 w-full rounded-sm border bg-background px-2 font-mono text-xs";
const btn = "flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary disabled:opacity-40";
</script>

<template>
  <div class="overflow-auto p-4">
    <ErrorLine v-if="error" :error="error" class="mb-3" />

    <section v-if="state" class="mb-6">
      <h3 class="mb-2 font-semibold">{{ t("sequence.state") }}</h3>
      <dl class="grid grid-cols-[12rem_1fr] gap-x-3 gap-y-1">
        <dt class="text-muted-foreground">{{ t("sequence.nextNotCached") }}</dt>
        <dd class="font-mono">{{ state.nextNotCached }}</dd>
        <dt class="text-muted-foreground">{{ t("sequence.cycleCount") }}</dt>
        <dd class="font-mono">{{ state.cycleCount }}</dd>
      </dl>
      <p class="mt-1 text-muted-foreground">{{ t("sequence.cacheHint") }}</p>
      <button :class="[btn, 'mt-2']" @click="load"><RefreshCw class="size-3.5" />{{ t("grid.reload") }}</button>
    </section>

    <section class="mb-6 max-w-xl">
      <h3 class="mb-2 font-semibold">{{ create ? t("sequence.new") : t("sequence.options") }}</h3>
      <form class="grid grid-cols-[12rem_1fr] items-center gap-x-3 gap-y-2" @submit.prevent="openPreview">
        <template v-if="create">
          <label for="seq-name" class="text-muted-foreground">{{ t("sequence.name") }}</label>
          <input id="seq-name" v-model="name" required :class="input" />
        </template>
        <template v-for="f in fields" :key="f">
          <label :for="`seq-${f}`" class="text-muted-foreground">{{ t(`sequence.${f}`) }}</label>
          <input :id="`seq-${f}`" v-model="opts[f]" required inputmode="numeric" :class="input" />
        </template>
        <label for="seq-cycle" class="text-muted-foreground">{{ t("sequence.cycle") }}</label>
        <input id="seq-cycle" v-model="opts.cycle" type="checkbox" class="size-4 justify-self-start" />
        <template v-if="!create">
          <label for="seq-restart" class="text-muted-foreground">{{ t("sequence.restart") }}</label>
          <input id="seq-restart" v-model="opts.restart" inputmode="numeric" :placeholder="t('sequence.restartPlaceholder')" :class="input" />
        </template>
        <div class="col-start-2">
          <button type="submit" class="flex h-7 items-center gap-1.5 rounded-sm bg-primary px-3 font-semibold text-primary-foreground">
            <Save class="size-3.5" />{{ create ? t("sequence.create") : t("sequence.alter") }}
          </button>
        </div>
      </form>
    </section>

    <template v-if="!create">
      <section class="mb-6 max-w-xl">
        <h3 class="mb-2 font-semibold">{{ t("sequence.setval") }}</h3>
        <form class="flex items-center gap-2" @submit.prevent="setval">
          <input v-model="nextValue" required inputmode="numeric" :aria-label="t('sequence.setval')" :class="[input, 'max-w-60']" />
          <button type="submit" :class="btn">{{ t("sequence.setvalRun") }}</button>
        </form>
        <p v-if="setvalResult === 'ok'" class="mt-1 text-success">{{ t("sequence.setvalOk") }}</p>
        <p v-else-if="setvalResult === 'ignored'" class="mt-1 text-warning">{{ t("sequence.setvalIgnored") }}</p>
      </section>

      <section class="max-w-xl">
        <h3 class="mb-2 font-semibold">{{ t("sequence.asDefault") }}</h3>
        <div class="flex items-center gap-2">
          <code class="min-w-0 flex-1 truncate rounded-sm border bg-card px-2 py-1 font-mono text-xs select-text">{{ defaultExpr }}</code>
          <button :class="btn" @click="copyDefault"><Copy class="size-3.5" />{{ copied ? t("sequence.copied") : t("sequence.copy") }}</button>
        </div>
        <p class="mt-1 text-muted-foreground">{{ t("sequence.asDefaultHint") }}</p>
      </section>
    </template>

    <dialog ref="dialog" class="m-auto w-[min(48rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <div class="border-b px-4 py-2 font-semibold">{{ t("grid.previewTitle") }}</div>
      <pre class="max-h-[40vh] overflow-auto p-4 font-mono text-xs whitespace-pre-wrap select-text">{{ preview }}</pre>
      <p class="px-4 text-muted-foreground">{{ t("sequence.ddlHint") }}</p>
      <ErrorLine v-if="applyError" :error="applyError" class="mx-4 mt-2" />
      <div class="flex justify-end gap-2 px-4 py-3">
        <button class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="dialog?.close()">{{ t("grid.cancel") }}</button>
        <button :disabled="busy" class="h-7 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50" @click="apply">{{ t("grid.apply") }}</button>
      </div>
    </dialog>
  </div>
</template>
