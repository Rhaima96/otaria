<script setup lang="ts">
import { ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { TriangleAlert } from "lucide-vue-next";
import { useConfirm } from "@/composables/useConfirm";

const { t } = useI18n();
const { pending } = useConfirm();
const dialog = ref<HTMLDialogElement | null>(null);

watch(pending, (p) => {
  if (!p || dialog.value?.open) return;
  dialog.value!.returnValue = "";
  dialog.value!.showModal();
});

// method="dialog": the clicked button's value becomes returnValue; Escape leaves it empty.
function closed() {
  pending.value?.resolve(dialog.value?.returnValue === "ok");
  pending.value = null;
}
</script>

<template>
  <dialog ref="dialog" class="m-auto w-[min(26rem,90vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40" @close="closed">
    <form method="dialog">
      <p class="flex gap-2 p-4">
        <TriangleAlert class="size-4 shrink-0 text-warning" />
        {{ pending?.message }}
      </p>
      <div class="flex justify-end gap-2 px-4 pb-3">
        <button value="" autofocus class="h-7 rounded-sm border px-3 font-medium hover:bg-secondary">{{ t("grid.cancel") }}</button>
        <button value="ok" class="h-7 rounded-sm bg-primary px-3 font-semibold text-primary-foreground hover:opacity-90">{{ pending?.ok }}</button>
      </div>
    </form>
  </dialog>
</template>
