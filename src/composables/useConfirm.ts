import { ref } from "vue";

// One app-wide confirmation dialog (ConfirmDialog.vue, mounted in App.vue), themed like the others.
interface Pending {
  message: string;
  ok: string;
  resolve: (yes: boolean) => void;
}
const pending = ref<Pending | null>(null);

/** Resolves true on the OK button, false on Cancel / Escape. */
function confirm(message: string, ok: string): Promise<boolean> {
  pending.value?.resolve(false);
  return new Promise((resolve) => (pending.value = { message, ok, resolve }));
}

export function useConfirm() {
  return { pending, confirm };
}
