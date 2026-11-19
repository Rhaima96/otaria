<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { CheckCircle2, Eye, EyeOff, Loader2, ShieldAlert, XCircle } from "lucide-vue-next";
import { api, isAppError, type AppError, type Profile, type ServerInfo, type TlsMode } from "@/lib/api";
import { TAG_COLORS, tagClass } from "@/lib/tagColors";
import { useSessions } from "@/composables/useSessions";
import { cn } from "@/lib/utils";

const props = defineProps<{ profile: Profile }>();
const emit = defineEmits<{ saved: [Profile]; deleted: [] }>();

const { t } = useI18n();
const { addSession, reloadProfiles } = useSessions();

// Plain deep copy (structuredClone throws on Vue reactive proxies).
const copy = (p: Profile): Profile => JSON.parse(JSON.stringify(p));
const form = ref<Profile>(copy(props.profile));
const password = ref("");
const sshSecret = ref("");
/** Show the typed secret; a saved one stays in the keychain and is never shown. */
const reveal = ref({ password: false, ssh: false });
const busy = ref<"" | "test" | "save" | "connect">("");
const result = ref<{ ok: true; info: ServerInfo } | { ok: false; error: AppError } | null>(null);

watch(
  () => props.profile,
  (p) => {
    form.value = copy(p);
    password.value = sshSecret.value = "";
    result.value = null;
  },
);

const hasSavedSecret = computed(() => !!props.profile.id && props.profile.savePassword);
const sshEnabled = computed({
  get: () => form.value.ssh !== null,
  set: (on) => (form.value.ssh = on ? { host: "", port: 22, user: "", keyPath: null, certPath: null, hostKey: null } : null),
});
const tlsModes: TlsMode[] = ["disabled", "preferred", "required", "verify-ca", "verify-identity"];
const canSubmit = computed(() => form.value.name.trim() && (form.value.host.trim() || form.value.socket?.trim()));

/** Empty field = keep the saved secret. */
const secrets = () => ({ password: password.value || null, sshSecret: sshSecret.value || null });

function fail(e: unknown) {
  result.value = { ok: false, error: isAppError(e) ? e : { code: "UNKNOWN", message: String(e), serverCode: null, detail: null } };
}

async function run(kind: "test" | "save" | "connect") {
  busy.value = kind;
  result.value = null;
  try {
    if (kind === "test") {
      result.value = { ok: true, info: await api.testConnection(form.value, secrets()) };
      return;
    }
    const saved = await api.saveProfile(form.value, secrets());
    form.value = copy(saved);
    await reloadProfiles();
    emit("saved", saved);
    if (kind === "connect") addSession(await api.connect(saved.id, secrets()));
  } catch (e) {
    fail(e);
  } finally {
    busy.value = "";
  }
}

/** The user accepted the SSH server's fingerprint: remember it on the profile and retry. */
function trustHostKey(fingerprint: string) {
  if (form.value.ssh) form.value.ssh.hostKey = fingerprint;
  run("test");
}

async function remove() {
  if (!form.value.id || !confirm(t("connection.confirmDelete", { name: form.value.name }))) return;
  await api.deleteProfile(form.value.id);
  await reloadProfiles();
  emit("deleted");
}

const input = "h-7 w-full min-w-0 rounded-sm border bg-background px-2 text-foreground placeholder:text-muted-foreground/70";
const label = "pt-1.5 text-right text-muted-foreground";
</script>

<template>
  <form class="flex max-w-2xl flex-col gap-5 p-5" @submit.prevent="run('connect')">
    <section class="grid grid-cols-[9rem_1fr] items-start gap-x-3 gap-y-2">
      <h2 class="col-span-2 font-semibold">{{ t("connection.general") }}</h2>

      <label :class="label" for="f-name">{{ t("connection.name") }}</label>
      <div class="flex items-center gap-2">
        <input id="f-name" v-model="form.name" :class="input" required :placeholder="t('connection.namePlaceholder')" />
        <div role="radiogroup" :aria-label="t('connection.color')" class="flex shrink-0 gap-1">
          <button
            v-for="c in [null, ...Object.keys(TAG_COLORS)]"
            :key="c ?? 'none'"
            type="button"
            role="radio"
            :aria-checked="form.color === c"
            :title="c ? t(`colors.${c}`) : t('colors.none')"
            :class="cn('size-4 rounded-full ring-offset-2 ring-offset-background', tagClass(c), form.color === c && 'ring-2 ring-ring')"
            @click="form.color = c"
          />
        </div>
      </div>

      <label :class="label" for="f-host">{{ t("connection.host") }}</label>
      <div class="flex gap-2">
        <input id="f-host" v-model="form.host" :class="input" spellcheck="false" />
        <input v-model.number="form.port" :aria-label="t('connection.port')" type="number" min="1" max="65535" :class="cn(input, 'w-20 shrink-0')" />
      </div>

      <label :class="label" for="f-user">{{ t("connection.user") }}</label>
      <input id="f-user" v-model="form.user" :class="input" spellcheck="false" autocomplete="off" />

      <label :class="label" for="f-pass">{{ t("connection.password") }}</label>
      <div class="flex flex-col gap-1">
        <div class="relative">
          <input
            id="f-pass"
            v-model="password"
            :type="reveal.password ? 'text' : 'password'"
            :class="cn(input, 'w-full pr-7')"
            autocomplete="off"
            spellcheck="false"
            :placeholder="hasSavedSecret ? t('connection.passwordSaved') : ''"
          />
          <button
            type="button"
            :title="reveal.password ? t('connection.hidePassword') : t('connection.showPassword')"
            :aria-pressed="reveal.password"
            class="absolute inset-y-0 right-0 flex w-7 items-center justify-center text-muted-foreground hover:text-foreground"
            @click="reveal.password = !reveal.password"
          >
            <component :is="reveal.password ? EyeOff : Eye" class="size-3.5" />
            <span class="sr-only">{{ reveal.password ? t("connection.hidePassword") : t("connection.showPassword") }}</span>
          </button>
        </div>
        <label class="flex items-center gap-1.5 text-muted-foreground">
          <input v-model="form.savePassword" type="checkbox" class="accent-primary" />
          {{ t("connection.savePassword") }}
        </label>
      </div>

      <label :class="label" for="f-db">{{ t("connection.database") }}</label>
      <input id="f-db" v-model="form.database" :class="input" spellcheck="false" :placeholder="t('connection.optional')" />

      <label :class="label" for="f-socket">{{ t("connection.socket") }}</label>
      <input id="f-socket" v-model="form.socket" :class="cn(input, 'font-mono')" spellcheck="false" :placeholder="t('connection.socketPlaceholder')" />
    </section>

    <section class="grid grid-cols-[9rem_1fr] items-start gap-x-3 gap-y-2">
      <h2 class="col-span-2 font-semibold">{{ t("connection.tls") }}</h2>
      <label :class="label" for="f-tls">{{ t("connection.tlsMode") }}</label>
      <div class="flex flex-col gap-1">
        <select id="f-tls" v-model="form.tls.mode" :class="input">
          <option v-for="m in tlsModes" :key="m" :value="m">{{ t(`tlsModes.${m}`) }}</option>
        </select>
        <span class="text-xs text-muted-foreground">{{ t(`tlsHelp.${form.tls.mode}`) }}</span>
      </div>
      <template v-if="form.tls.mode !== 'disabled'">
        <label :class="label" for="f-ca">{{ t("connection.tlsCa") }}</label>
        <input id="f-ca" v-model="form.tls.ca" :class="cn(input, 'font-mono')" spellcheck="false" :placeholder="t('connection.filePath')" />
        <label :class="label" for="f-cert">{{ t("connection.tlsCert") }}</label>
        <div class="flex gap-2">
          <input id="f-cert" v-model="form.tls.cert" :class="cn(input, 'font-mono')" spellcheck="false" :placeholder="t('connection.tlsCertPlaceholder')" />
          <input v-model="form.tls.key" :aria-label="t('connection.tlsKey')" :class="cn(input, 'font-mono')" spellcheck="false" :placeholder="t('connection.tlsKeyPlaceholder')" />
        </div>
      </template>
    </section>

    <section class="grid grid-cols-[9rem_1fr] items-start gap-x-3 gap-y-2">
      <h2 class="col-span-2 flex items-center gap-2 font-semibold">
        <label class="flex items-center gap-1.5">
          <input v-model="sshEnabled" type="checkbox" class="accent-primary" />
          {{ t("connection.ssh") }}
        </label>
      </h2>
      <template v-if="form.ssh">
        <label :class="label" for="f-ssh-host">{{ t("connection.sshHost") }}</label>
        <div class="flex gap-2">
          <input id="f-ssh-host" v-model="form.ssh.host" :class="input" spellcheck="false" />
          <input v-model.number="form.ssh.port" :aria-label="t('connection.port')" type="number" min="1" max="65535" :class="cn(input, 'w-20 shrink-0')" />
        </div>
        <label :class="label" for="f-ssh-user">{{ t("connection.user") }}</label>
        <input id="f-ssh-user" v-model="form.ssh.user" :class="input" spellcheck="false" autocomplete="off" />
        <label :class="label" for="f-ssh-key">{{ t("connection.sshKey") }}</label>
        <input id="f-ssh-key" v-model="form.ssh.keyPath" :class="cn(input, 'font-mono')" spellcheck="false" :placeholder="t('connection.sshKeyPlaceholder')" />
        <template v-if="form.ssh.keyPath">
          <label :class="label" for="f-ssh-cert">{{ t("connection.sshCert") }}</label>
          <input
            id="f-ssh-cert"
            :value="form.ssh.certPath ?? ''"
            :class="cn(input, 'font-mono')"
            spellcheck="false"
            :placeholder="t('connection.sshCertPlaceholder')"
            @input="form.ssh.certPath = ($event.target as HTMLInputElement).value || null"
          />
        </template>
        <label :class="label" for="f-ssh-secret">{{ form.ssh.keyPath ? t("connection.sshPassphrase") : t("connection.password") }}</label>
        <div class="relative">
          <input
            id="f-ssh-secret"
            v-model="sshSecret"
            :type="reveal.ssh ? 'text' : 'password'"
            :class="cn(input, 'w-full pr-7')"
            autocomplete="off"
            spellcheck="false"
            :placeholder="hasSavedSecret ? t('connection.passwordSaved') : ''"
          />
          <button
            type="button"
            :title="reveal.ssh ? t('connection.hidePassword') : t('connection.showPassword')"
            :aria-pressed="reveal.ssh"
            class="absolute inset-y-0 right-0 flex w-7 items-center justify-center text-muted-foreground hover:text-foreground"
            @click="reveal.ssh = !reveal.ssh"
          >
            <component :is="reveal.ssh ? EyeOff : Eye" class="size-3.5" />
            <span class="sr-only">{{ reveal.ssh ? t("connection.hidePassword") : t("connection.showPassword") }}</span>
          </button>
        </div>
        <template v-if="form.ssh.hostKey">
          <span :class="label">{{ t("connection.sshHostKey") }}</span>
          <span class="truncate pt-1.5 font-mono text-xs text-muted-foreground" :title="form.ssh.hostKey">{{ form.ssh.hostKey }}</span>
        </template>
      </template>
    </section>

    <div v-if="result" role="status" class="rounded-md border p-3">
      <template v-if="result.ok">
        <p class="flex items-center gap-1.5 font-medium text-success">
          <CheckCircle2 class="size-4" /> {{ t("connection.testOk") }}
        </p>
        <p class="mt-1 font-mono text-xs">{{ result.info.version }} · {{ result.info.currentUser }}</p>
        <p class="font-mono text-xs text-muted-foreground">
          {{ result.info.tlsCipher ? t("session.tlsOn", { cipher: result.info.tlsCipher }) : t("session.tlsOff") }}
        </p>
        <p v-if="!result.info.isMariadb" class="mt-1 text-warning">⚠ {{ t("session.notMariadb") }}</p>
      </template>
      <template v-else>
        <p class="flex items-center gap-1.5 font-medium text-destructive">
          <component :is="result.error.code.startsWith('SSH_HOST') ? ShieldAlert : XCircle" class="size-4 shrink-0" />
          {{ t(`errors.${result.error.code}`, t("errors.UNKNOWN")) }}
        </p>
        <p class="mt-1 font-mono text-xs break-words text-muted-foreground">
          <span v-if="result.error.serverCode">[{{ result.error.serverCode }}] </span>{{ result.error.message }}
        </p>
        <div v-if="result.error.detail && result.error.code.startsWith('SSH_HOST')" class="mt-2 flex flex-col items-start gap-2">
          <code class="font-mono text-xs">{{ result.error.detail }}</code>
          <!-- A refused host certificate shows the fingerprint of its authority: trusted through known_hosts, not here. -->
          <button
            v-if="result.error.code !== 'SSH_HOST_CERT'"
            type="button"
            class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary"
            @click="trustHostKey(result.error.detail)"
          >
            {{ t("connection.trustHostKey") }}
          </button>
        </div>
      </template>
    </div>

    <div class="flex items-center gap-2">
      <button type="submit" :disabled="!canSubmit || !!busy" class="flex h-7 items-center gap-1.5 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50">
        <Loader2 v-if="busy === 'connect'" class="size-3.5 animate-spin" />
        {{ t("connection.connect") }}
      </button>
      <button type="button" :disabled="!canSubmit || !!busy" class="flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary disabled:opacity-50" @click="run('test')">
        <Loader2 v-if="busy === 'test'" class="size-3.5 animate-spin" />
        {{ t("connection.test") }}
      </button>
      <button type="button" :disabled="!canSubmit || !!busy" class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary disabled:opacity-50" @click="run('save')">
        {{ t("connection.save") }}
      </button>
      <button v-if="form.id" type="button" class="ml-auto h-7 rounded-sm px-3 font-medium text-destructive hover:bg-secondary" @click="remove">
        {{ t("connection.delete") }}
      </button>
    </div>
  </form>
</template>
