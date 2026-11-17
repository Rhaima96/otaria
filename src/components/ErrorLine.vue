<script setup lang="ts">
import { useI18n } from "vue-i18n";
import { XCircle } from "lucide-vue-next";
import type { AppError } from "@/lib/api";

defineProps<{ error: AppError }>();
const { t } = useI18n();
</script>

<template>
  <div role="alert" class="rounded-sm border border-destructive/40 bg-destructive/10 p-2 text-destructive">
    <p class="flex items-center gap-1.5 font-medium">
      <XCircle class="size-4 shrink-0" />
      {{ t(`errors.${error.code}`, t("errors.UNKNOWN")) }}
    </p>
    <p class="mt-1 font-mono text-xs break-words whitespace-pre-wrap">
      <span v-if="error.serverCode">[{{ error.serverCode }}] </span>{{ error.message }}
    </p>
  </div>
</template>
