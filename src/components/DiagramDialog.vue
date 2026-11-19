<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { save } from "@tauri-apps/plugin-dialog";
import { Check, Copy, Image, Maximize, Minus, Plus, Save } from "lucide-vue-next";
import ErrorLine from "@/components/ErrorLine.vue";
import { useTheme } from "@/composables/useTheme";
import { api, toAppError, type AppError } from "@/lib/api";

/** Mermaid `erDiagram` text for a schema: pick tables, then copy or save as .mmd. */
const props = defineProps<{ sessionId: string }>();
const { t } = useI18n();
const { dark } = useTheme();

const dialog = ref<HTMLDialogElement | null>(null);
const schema = ref("");
const tables = ref<string[]>([]);
const picked = ref<string[]>([]);
const neighbors = ref(false);
const filter = ref("");
const text = ref("");
const copied = ref(false);
const result = ref("");
const error = ref<AppError | null>(null);
const view = ref<"diagram" | "text">("diagram");
const svg = ref("");
const size = ref({ w: 0, h: 0 });
const zoom = ref(1);
const pane = ref<HTMLDivElement | null>(null);

const shown = computed(() => tables.value.filter((x) => x.toLowerCase().includes(filter.value.toLowerCase())));

async function open(s: string) {
  schema.value = s;
  tables.value = [];
  picked.value = [];
  neighbors.value = false;
  filter.value = "";
  text.value = "";
  result.value = "";
  error.value = null;
  view.value = "diagram";
  dialog.value?.showModal();
  try {
    tables.value = (await api.listObjects(props.sessionId, s)).filter((o) => o.kind === "table").map((o) => o.name);
    picked.value = [...tables.value];
  } catch (e) {
    error.value = toAppError(e);
  }
}
defineExpose({ open });

// Regenerate on every change; a slower earlier answer never overwrites a newer one.
let seq = 0;
watch([picked, neighbors], async () => {
  const n = ++seq;
  copied.value = false;
  result.value = "";
  if (!picked.value.length) return (text.value = "");
  try {
    const out = await api.erDiagram(props.sessionId, schema.value, picked.value, neighbors.value);
    if (n === seq) (text.value = out), (error.value = null);
  } catch (e) {
    if (n === seq) error.value = toAppError(e);
  }
});

// Mermaid is loaded on first render only (its own chunk, bundled: works offline).
let mermaid: Promise<typeof import("mermaid").default> | null = null;
let drawSeq = 0;
// Redrawn when the theme changes too: the colours are baked into the SVG.
watch([text, dark], async ([src]) => {
  const n = ++drawSeq;
  if (!src) return (svg.value = "");
  try {
    const m = await (mermaid ??= import("mermaid").then((x) => x.default));
    const css = getComputedStyle(document.documentElement);
    const v = (name: string) => css.getPropertyValue(`--${name}`).trim();
    // Colours are read from tokens.css at render time, so the diagram follows the current theme.
    m.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      theme: "base",
      htmlLabels: false, // plain SVG text: no foreignObject, so the PNG export canvas is not tainted
      fontFamily: "IBM Plex Sans, sans-serif",
      themeVariables: {
        background: v("background"),
        mainBkg: v("card"),
        primaryColor: v("card"),
        primaryTextColor: v("foreground"),
        primaryBorderColor: v("border"),
        secondaryColor: v("secondary"),
        tertiaryColor: v("background"),
        lineColor: v("muted-foreground"),
        textColor: v("foreground"),
        nodeBorder: v("border"),
        rowOdd: v("background"),
        rowEven: v("card"),
      },
    });
    const out = (await m.render(`er-${n}`, src)).svg;
    if (n !== drawSeq) return;
    const box = /viewBox="[\d.-]+ [\d.-]+ ([\d.]+) ([\d.]+)"/.exec(out);
    size.value = { w: Number(box?.[1] ?? 800), h: Number(box?.[2] ?? 600) };
    svg.value = out;
    fit();
  } catch (e) {
    if (n === drawSeq) (svg.value = ""), (error.value = toAppError(e));
  }
});

function fit() {
  const w = (pane.value?.clientWidth ?? size.value.w) - 16;
  zoom.value = Math.min(1, w / size.value.w);
}
const setZoom = (f: number) => (zoom.value = Math.min(4, Math.max(0.1, zoom.value * f)));

/** The rendered SVG with fixed pixel size, for files and the PNG canvas. */
function sizedSvg() {
  const doc = new DOMParser().parseFromString(svg.value, "image/svg+xml");
  const el = doc.documentElement;
  el.setAttribute("width", String(size.value.w));
  el.setAttribute("height", String(size.value.h));
  el.removeAttribute("style");
  return new XMLSerializer().serializeToString(el);
}

async function pngBytes() {
  const img = new window.Image();
  img.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(sizedSvg())}`;
  await img.decode();
  const scale = 2;
  const canvas = document.createElement("canvas");
  canvas.width = size.value.w * scale;
  canvas.height = size.value.h * scale;
  const ctx = canvas.getContext("2d")!;
  ctx.fillStyle = getComputedStyle(document.documentElement).getPropertyValue("--background").trim();
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
  const blob = await new Promise<Blob | null>((ok) => canvas.toBlob(ok, "image/png"));
  return new Uint8Array(await blob!.arrayBuffer());
}

/** All / none act on the filtered list only. */
function pickShown(on: boolean) {
  const set = new Set(picked.value);
  for (const x of shown.value) on ? set.add(x) : set.delete(x);
  picked.value = tables.value.filter((x) => set.has(x));
}

async function copy() {
  await navigator.clipboard.writeText(text.value);
  copied.value = true;
}

async function saveFile(ext: "mmd" | "svg" | "png") {
  const name = { mmd: "Mermaid", svg: "SVG", png: "PNG" }[ext];
  const path = await save({ defaultPath: `${schema.value}.${ext}`, filters: [{ name, extensions: [ext] }] });
  if (!path) return;
  try {
    await api.saveDiagram(path, ext === "mmd" ? text.value : ext === "svg" ? sizedSvg() : await pngBytes());
    result.value = t("diagram.saved", { path });
  } catch (e) {
    error.value = toAppError(e);
  }
}

const btn = "flex h-7 items-center gap-1.5 rounded-sm border px-3 font-medium hover:bg-secondary disabled:opacity-50";
const field = "h-7 rounded-sm border bg-background px-2";
</script>

<template>
  <dialog ref="dialog" class="m-auto w-[min(60rem,94vw)] rounded-lg border bg-popover p-0 text-popover-foreground backdrop:bg-black/40">
    <div class="border-b px-4 py-2 font-semibold">{{ t("diagram.title", { schema }) }}</div>

    <div class="grid h-[65vh] grid-cols-[16rem_1fr] gap-3 p-4">
      <div class="flex min-h-0 flex-col gap-2">
        <input v-model="filter" :placeholder="t('diagram.filter')" :aria-label="t('diagram.filter')" :class="field" />
        <div class="flex items-center gap-2 text-muted-foreground">
          <button class="hover:text-foreground hover:underline" @click="pickShown(true)">{{ t("diagram.all") }}</button>
          <span>·</span>
          <button class="hover:text-foreground hover:underline" @click="pickShown(false)">{{ t("diagram.none") }}</button>
          <span class="ml-auto">{{ t("diagram.count", { n: picked.length, total: tables.length }) }}</span>
        </div>
        <ul class="min-h-0 flex-1 overflow-auto rounded-sm border p-1">
          <li v-for="x in shown" :key="x">
            <label class="flex items-center gap-1.5 rounded-sm px-1 py-0.5 hover:bg-secondary">
              <input v-model="picked" type="checkbox" :value="x" class="size-3.5" /><span class="truncate">{{ x }}</span>
            </label>
          </li>
          <li v-if="!tables.length && !error" class="px-1 text-muted-foreground">{{ t("diagram.noTables") }}</li>
        </ul>
        <label class="flex items-start gap-1.5"><input v-model="neighbors" type="checkbox" class="mt-0.5 size-3.5" />{{ t("diagram.neighbors") }}</label>
      </div>
      <div class="flex min-h-0 flex-col gap-2">
        <div class="flex items-center gap-1">
          <button v-for="x in (['diagram', 'text'] as const)" :key="x" :class="[btn, view === x && 'bg-secondary']" :aria-pressed="view === x" @click="view = x">
            {{ x === "diagram" ? t("diagram.viewDiagram") : t("diagram.viewText") }}
          </button>
          <template v-if="view === 'diagram'">
            <span class="ml-auto tabular-nums text-muted-foreground">{{ Math.round(zoom * 100) }} %</span>
            <button :class="btn" :title="t('diagram.zoomOut')" :aria-label="t('diagram.zoomOut')" @click="setZoom(1 / 1.25)"><Minus class="size-3.5" /></button>
            <button :class="btn" :title="t('diagram.zoomIn')" :aria-label="t('diagram.zoomIn')" @click="setZoom(1.25)"><Plus class="size-3.5" /></button>
            <button :class="btn" :title="t('diagram.fit')" :aria-label="t('diagram.fit')" @click="fit"><Maximize class="size-3.5" /></button>
          </template>
        </div>
        <div v-show="view === 'diagram'" ref="pane" class="min-h-0 flex-1 overflow-auto rounded-sm border bg-background p-2">
          <div v-if="svg" class="[&>svg]:h-auto [&>svg]:w-full [&>svg]:max-w-none" :style="{ width: `${size.w * zoom}px` }" v-html="svg" />
          <p v-else-if="text" class="text-muted-foreground">{{ t("diagram.rendering") }}</p>
        </div>
        <template v-if="view === 'text'">
          <textarea :value="text" readonly :aria-label="t('diagram.title', { schema })" class="min-h-0 flex-1 resize-none rounded-sm border bg-card p-2 font-mono text-xs" />
          <p class="text-muted-foreground">{{ t("diagram.hint") }}</p>
        </template>
        <ErrorLine v-if="error" :error="error" />
        <p v-if="result" role="status" class="break-all text-success">{{ result }}</p>
      </div>
    </div>

    <div class="flex justify-end gap-2 border-t px-4 py-3">
      <button :class="btn" @click="dialog?.close()">{{ t("diagram.close") }}</button>
      <button :disabled="!text" :class="btn" @click="copy">
        <component :is="copied ? Check : Copy" class="size-3.5" />{{ copied ? t("diagram.copied") : t("diagram.copy") }}
      </button>
      <button :disabled="!svg" :class="btn" @click="saveFile('svg')"><Image class="size-3.5" />{{ t("diagram.saveSvg") }}</button>
      <button :disabled="!svg" :class="btn" @click="saveFile('png')"><Image class="size-3.5" />{{ t("diagram.savePng") }}</button>
      <button :disabled="!text" class="flex h-7 items-center gap-1.5 rounded-sm bg-primary px-3 font-semibold text-primary-foreground disabled:opacity-50" @click="saveFile('mmd')">
        <Save class="size-3.5" />{{ t("diagram.save") }}
      </button>
    </div>
  </dialog>
</template>
