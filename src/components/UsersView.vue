<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { Lock, RefreshCw, Shield, Trash2, User, UserPlus } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import GrantsEditor from "@/components/GrantsEditor.vue";
import { api, toAppError, type Account, type AccountForm, type AppError, type Privilege } from "@/lib/api";
import { cn } from "@/lib/utils";

const props = defineProps<{ sessionId: string; active?: boolean; mariadb: boolean }>();
const { t } = useI18n();

const accounts = ref<Account[]>([]);
const privileges = ref<Privilege[]>([]);
const plugins = ref<string[]>([]);
const error = ref<AppError | null>(null);
const filter = ref("");

async function load() {
  error.value = null;
  try {
    accounts.value = await api.listAccounts(props.sessionId);
    if (!privileges.value.length) [privileges.value, plugins.value] = await api.usersMeta(props.sessionId);
  } catch (e) {
    error.value = toAppError(e);
  }
}
onMounted(load);

const key = (a: { user: string; host: string; isRole: boolean }) => (a.isRole ? a.user : `${a.user}@${a.host}`);
const shown = computed(() => {
  const f = filter.value.trim().toLowerCase();
  return accounts.value.filter((a) => !f || key(a).toLowerCase().includes(f));
});
const roles = computed(() => accounts.value.filter((a) => a.isRole).map((a) => a.user));
/** Roles this account can hold (a role cannot hold itself). */
const grantable = computed(() => roles.value.filter((r) => !form.value?.isRole || r !== form.value.user));

// The account being edited; original = null for a new one.
const form = ref<AccountForm | null>(null);
const tab = ref<"account" | "privileges">("account");
const pluginChoices = computed(() => [...new Set([...plugins.value, form.value?.plugin ?? ""].filter(Boolean))]);

function select(a: Account) {
  form.value = { ...a, roles: [...a.roles], adminRoles: [...a.adminRoles], original: [a.user, a.host], password: null, expireNow: false };
  lifetimeMode.value = a.lifetime === null ? "default" : a.lifetime === 0 ? "never" : "days";
  days.value = a.lifetime || 90;
  tab.value = "account";
  formError.value = null;
}

function create(isRole: boolean) {
  const a: Account = {
    user: "",
    host: isRole ? "" : "%",
    isRole,
    plugin: isRole ? "" : "mysql_native_password",
    locked: false,
    expired: false,
    lifetime: null,
    maxQuestions: 0,
    maxUpdates: 0,
    maxConnections: 0,
    maxUserConnections: 0,
    maxStatementTime: 0,
    defaultRole: null,
    roles: [],
    adminRoles: [],
    require: "",
  };
  select(a);
  form.value!.original = null;
}

// Password lifetime: server default, never, or N days.
const lifetimeMode = ref<"default" | "never" | "days">("default");
const days = ref(90);

/** The form as sent: empty password = keep it (or none, for a new account: the backend asks for one). */
function payload(): AccountForm {
  const f = form.value!;
  // A default role must be one of the granted roles.
  const defaultRole = f.defaultRole && f.roles.includes(f.defaultRole) ? f.defaultRole : null;
  const adminRoles = f.adminRoles.filter((r) => f.roles.includes(r));
  return { ...f, defaultRole, adminRoles, password: f.password || null, lifetime: lifetimeMode.value === "default" ? null : lifetimeMode.value === "never" ? 0 : days.value };
}

// Preview, then apply.
const dialog = ref<HTMLDialogElement | null>(null);
const preview = ref<string[]>([]);
const formError = ref<AppError | null>(null);
const applyError = ref<AppError | null>(null);
const busy = ref(false);

async function review() {
  formError.value = null;
  applyError.value = null;
  try {
    preview.value = await api.accountSql(props.sessionId, payload());
    dialog.value?.showModal();
  } catch (e) {
    formError.value = toAppError(e);
  }
}

async function apply() {
  busy.value = true;
  applyError.value = null;
  const p = payload();
  try {
    await api.accountApply(props.sessionId, p);
    dialog.value?.close();
    await load();
    const again = accounts.value.find((a) => a.user === p.user && (p.isRole || a.host === p.host));
    if (again) select(again);
  } catch (e) {
    applyError.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

// DROP USER / ROLE: the name must be typed again.
const dropDialog = ref<HTMLDialogElement | null>(null);
const dropSql = ref("");
const typed = ref("");

async function askDrop() {
  const f = form.value!;
  dropSql.value = await api.accountDropSql(f.original![0], f.original![1], f.isRole);
  typed.value = "";
  applyError.value = null;
  dropDialog.value?.showModal();
}

async function drop() {
  const f = form.value!;
  busy.value = true;
  applyError.value = null;
  try {
    await api.accountDrop(props.sessionId, f.original![0], f.original![1], f.isRole);
    dropDialog.value?.close();
    form.value = null;
    await load();
  } catch (e) {
    applyError.value = toAppError(e);
  } finally {
    busy.value = false;
  }
}

const btn = "flex h-6 items-center gap-1 rounded-sm px-2 text-muted-foreground hover:bg-secondary hover:text-foreground";
const label = "text-right text-muted-foreground";
const input = "h-7 rounded-sm border bg-background px-2";
const tabBtn = "h-8 border-b-2 px-3";
</script>

<template>
  <div class="flex">
    <aside class="flex w-64 shrink-0 flex-col border-r">
      <div class="flex h-9 shrink-0 items-center gap-1 border-b px-2">
        <input v-model="filter" :placeholder="t('users.filter')" :aria-label="t('users.filter')" class="h-6 min-w-0 flex-1 rounded-sm border bg-background px-2" />
        <button :class="btn" :title="t('admin.refresh')" @click="load"><RefreshCw class="size-3.5" /><span class="sr-only">{{ t("admin.refresh") }}</span></button>
      </div>
      <div class="flex flex-wrap gap-1 border-b p-2 whitespace-nowrap">
        <button :class="btn" @click="create(false)"><UserPlus class="size-3.5" />{{ t("users.newUser") }}</button>
        <!-- Roles: MariaDB only (MySQL roles are not handled here). -->
        <button v-if="mariadb" :class="btn" @click="create(true)"><Shield class="size-3.5" />{{ t("users.newRole") }}</button>
      </div>
      <ErrorLine v-if="error" :error="error" class="m-2" />
      <ul class="min-h-0 flex-1 overflow-auto py-1">
        <li v-for="a in shown" :key="key(a)">
          <button
            :class="cn('flex w-full items-center gap-1.5 px-3 py-1 text-left hover:bg-secondary', form?.original && key(a) === key({ user: form.original[0], host: form.original[1], isRole: form.isRole }) && 'bg-selection')"
            @click="select(a)"
          >
            <component :is="a.isRole ? Shield : User" class="size-3.5 shrink-0 text-muted-foreground" />
            <span class="truncate font-mono text-xs">{{ key(a) }}</span>
            <Lock v-if="a.locked" class="ml-auto size-3 shrink-0 text-warning" :aria-label="t('users.locked')" />
          </button>
        </li>
      </ul>
    </aside>

    <section v-if="form" class="flex min-w-0 flex-1 flex-col">
      <div class="flex h-9 shrink-0 items-center gap-2 border-b px-3">
        <component :is="form.isRole ? Shield : User" class="size-4 text-muted-foreground" />
        <span class="font-mono font-semibold">{{ form.original ? key({ user: form.original[0], host: form.original[1], isRole: form.isRole }) : form.isRole ? t("users.newRole") : t("users.newUser") }}</span>
        <button v-if="form.original" :class="cn(btn, 'ml-auto hover:text-destructive')" @click="askDrop"><Trash2 class="size-3.5" />{{ t("users.drop") }}</button>
      </div>
      <nav v-if="form.original" class="flex shrink-0 border-b px-2" role="tablist">
        <button role="tab" :aria-selected="tab === 'account'" :class="cn(tabBtn, tab === 'account' ? 'border-primary font-medium' : 'border-transparent text-muted-foreground')" @click="tab = 'account'">
          {{ form.isRole ? t("users.roleTab") : t("users.accountTab") }}
        </button>
        <button role="tab" :aria-selected="tab === 'privileges'" :class="cn(tabBtn, tab === 'privileges' ? 'border-primary font-medium' : 'border-transparent text-muted-foreground')" @click="tab = 'privileges'">
          {{ t("users.privilegesTab") }}
        </button>
      </nav>

      <form v-show="tab === 'account'" class="min-h-0 flex-1 overflow-auto p-4" @submit.prevent="review">
        <div class="grid max-w-2xl grid-cols-[11rem_1fr] items-center gap-x-3 gap-y-2">
          <label :class="label" for="u-user">{{ form.isRole ? t("users.roleName") : t("users.user") }}</label>
          <input id="u-user" v-model="form.user" :disabled="form.isRole && !!form.original" :class="cn(input, 'font-mono')" />

          <template v-if="!form.isRole">
            <label :class="label" for="u-host">{{ t("users.host") }}</label>
            <div>
              <input id="u-host" v-model="form.host" :class="cn(input, 'w-full font-mono')" />
              <p class="mt-0.5 text-xs text-muted-foreground">{{ t("users.hostHint") }}</p>
            </div>

            <label :class="label" for="u-plugin">{{ t("users.plugin") }}</label>
            <select id="u-plugin" v-model="form.plugin" :class="cn(input, 'font-mono')">
              <option v-for="p in pluginChoices" :key="p" :value="p">{{ p }}</option>
            </select>

            <label :class="label" for="u-pw">{{ t("users.password") }}</label>
            <input id="u-pw" v-model="form.password" type="password" autocomplete="new-password" :placeholder="form.original ? t('users.passwordKeep') : ''" :class="input" />

            <span :class="label">{{ t("users.lifetime") }}</span>
            <div class="flex items-center gap-2">
              <select v-model="lifetimeMode" :aria-label="t('users.lifetime')" :class="input">
                <option value="default">{{ t("users.lifetimeDefault") }}</option>
                <option value="never">{{ t("users.lifetimeNever") }}</option>
                <option value="days">{{ t("users.lifetimeDays") }}</option>
              </select>
              <input v-if="lifetimeMode === 'days'" v-model.number="days" type="number" min="1" :aria-label="t('users.days')" :class="cn(input, 'w-24')" />
              <span v-if="lifetimeMode === 'days'" class="text-muted-foreground">{{ t("users.days") }}</span>
            </div>

            <span :class="label">{{ t("users.state") }}</span>
            <div class="flex flex-wrap items-center gap-4">
              <label class="flex items-center gap-1.5"><input v-model="form.locked" type="checkbox" class="size-3.5" />{{ t("users.locked") }}</label>
              <label class="flex items-center gap-1.5"><input v-model="form.expireNow" type="checkbox" class="size-3.5" />{{ t("users.expireNow") }}</label>
              <span v-if="form.expired" class="text-warning">{{ t("users.expired") }}</span>
            </div>

            <label :class="label" for="u-req">{{ t("users.require") }}</label>
            <div class="flex items-center gap-2">
              <select id="u-req" v-model="form.require" :disabled="form.require === 'SPECIFIED'" :class="input">
                <option value="">{{ t("users.requireNone") }}</option>
                <option value="SSL">{{ t("users.requireSsl") }}</option>
                <option value="X509">{{ t("users.requireX509") }}</option>
                <option v-if="form.require === 'SPECIFIED'" value="SPECIFIED">{{ t("users.requireSpecified") }}</option>
              </select>
              <span class="text-xs text-muted-foreground">{{ form.require === "SPECIFIED" ? t("users.requireSpecifiedHint") : t("users.requireHint") }}</span>
            </div>

            <span :class="cn(label, 'self-start pt-1.5')">{{ t("users.limits") }}</span>
            <div class="grid grid-cols-[auto_6rem] items-center gap-x-2 gap-y-1">
              <label for="u-mq">{{ t("users.maxQuestions") }}</label><input id="u-mq" v-model.number="form.maxQuestions" type="number" min="0" :class="input" />
              <label for="u-mu">{{ t("users.maxUpdates") }}</label><input id="u-mu" v-model.number="form.maxUpdates" type="number" min="0" :class="input" />
              <label for="u-mc">{{ t("users.maxConnections") }}</label><input id="u-mc" v-model.number="form.maxConnections" type="number" min="0" :class="input" />
              <label for="u-muc">{{ t("users.maxUserConnections") }}</label><input id="u-muc" v-model.number="form.maxUserConnections" type="number" min="0" :class="input" />
              <template v-if="mariadb">
                <label for="u-mst">{{ t("users.maxStatementTime") }}</label><input id="u-mst" v-model.number="form.maxStatementTime" type="number" min="0" step="0.1" :class="input" />
              </template>
              <p class="col-span-2 text-xs text-muted-foreground">{{ t("users.limitsHint") }}</p>
            </div>
          </template>

          <template v-if="mariadb">
            <span :class="cn(label, 'self-start pt-1')">{{ t("users.roles") }}</span>
            <div class="flex flex-wrap gap-x-4 gap-y-1">
              <span v-for="r in grantable" :key="r" class="flex items-center gap-1.5 font-mono text-xs">
                <label class="flex items-center gap-1.5"><input v-model="form.roles" type="checkbox" :value="r" class="size-3.5" />{{ r }}</label>
                <label v-if="form.roles.includes(r)" class="flex items-center gap-1 font-sans text-muted-foreground" :title="t('users.adminOptionHint')">
                  (<input v-model="form.adminRoles" type="checkbox" :value="r" class="size-3" />{{ t("users.adminOption") }})
                </label>
              </span>
              <span v-if="!grantable.length" class="text-muted-foreground">{{ t("users.noRoles") }}</span>
            </div>

            <template v-if="!form.isRole">
              <label :class="label" for="u-dr">{{ t("users.defaultRole") }}</label>
              <select id="u-dr" v-model="form.defaultRole" :class="cn(input, 'font-mono')">
                <option :value="null">{{ t("users.none") }}</option>
                <option v-for="r in form.roles" :key="r" :value="r">{{ r }}</option>
              </select>
            </template>
          </template>
        </div>
        <ErrorLine v-if="formError" :error="formError" class="mt-3 max-w-2xl" />
        <button type="submit" class="mt-4 h-7 rounded-sm bg-primary px-3 font-semibold text-primary-foreground">{{ t("users.review") }}</button>
      </form>

      <GrantsEditor
        v-if="form.original && tab === 'privileges'"
        :session-id="sessionId"
        :user="form.original[0]"
        :host="form.original[1]"
        :is-role="form.isRole"
        :privileges="privileges"
        class="min-h-0 flex-1"
      />
    </section>
    <p v-else class="p-5 text-muted-foreground">{{ t("users.pick") }}</p>

    <dialog ref="dialog" class="m-auto w-[min(48rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <div class="border-b px-4 py-2 font-semibold">{{ t("grid.previewTitle") }}</div>
      <pre v-if="preview.length" class="max-h-[40vh] overflow-auto p-4 font-mono text-xs whitespace-pre-wrap select-text">{{ preview.join(";\n") + ";" }}</pre>
      <p v-else class="p-4 text-muted-foreground">{{ t("users.nothing") }}</p>
      <p v-if="preview.length" class="px-4 text-muted-foreground">{{ t("users.applyHint") }}</p>
      <ErrorLine v-if="applyError" :error="applyError" class="mx-4 mt-2" />
      <div class="flex justify-end gap-2 px-4 py-3">
        <button class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="dialog?.close()">{{ t("grid.cancel") }}</button>
        <button :disabled="busy || !preview.length" class="h-7 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50" @click="apply">{{ t("grid.apply") }}</button>
      </div>
    </dialog>

    <dialog ref="dropDialog" class="m-auto w-[min(32rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
      <form v-if="form?.original" @submit.prevent="drop">
        <div class="border-b px-4 py-2 font-semibold">{{ t("users.drop") }}</div>
        <pre class="p-4 font-mono text-xs select-text">{{ dropSql }}</pre>
        <label class="block px-4 text-muted-foreground" for="u-confirm">{{ t("users.dropConfirm", { name: form.original[0] }) }}</label>
        <input id="u-confirm" v-model="typed" autocomplete="off" :class="cn(input, 'mx-4 mt-1 w-[calc(100%-2rem)] font-mono')" />
        <ErrorLine v-if="applyError" :error="applyError" class="mx-4 mt-2" />
        <div class="flex justify-end gap-2 px-4 py-3">
          <button type="button" class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary" @click="dropDialog?.close()">{{ t("grid.cancel") }}</button>
          <button type="submit" :disabled="busy || typed !== form.original[0]" class="h-7 rounded-sm bg-destructive px-3 font-semibold text-primary-foreground disabled:opacity-40">{{ t("users.drop") }}</button>
        </div>
      </form>
    </dialog>
  </div>
</template>
