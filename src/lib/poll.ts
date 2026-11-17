import { onUnmounted, ref, watch } from "vue";

/** Runs `fn` now and every `every` seconds (0 = paused) while `active()`; never two calls at once. */
export function usePolling(fn: () => Promise<void>, active: () => boolean, every = ref(2)) {
  let timer: number | undefined;
  let busy = false;
  async function tick() {
    if (busy) return;
    busy = true;
    try {
      await fn();
    } finally {
      busy = false;
    }
  }
  watch(
    [active, every],
    () => {
      clearInterval(timer);
      if (!active()) return;
      tick();
      if (every.value > 0) timer = window.setInterval(tick, every.value * 1000);
    },
    { immediate: true },
  );
  onUnmounted(() => clearInterval(timer));
  return { every, tick };
}

/** Choices for the refresh-interval menu, in seconds (0 = paused). */
export const INTERVALS = [0, 1, 2, 5, 10];
