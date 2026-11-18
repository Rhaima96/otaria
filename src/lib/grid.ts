import { AllCommunityModule, ModuleRegistry, themeQuartz } from "ag-grid-community";

ModuleRegistry.registerModules([AllCommunityModule]);

/** AG Grid theme on the app tokens, shared by the table grid and the SQL results. */
export const theme = themeQuartz.withParams({
  backgroundColor: "var(--background)",
  foregroundColor: "var(--foreground)",
  borderColor: "var(--border)",
  chromeBackgroundColor: "var(--sidebar)",
  accentColor: "var(--primary)",
  selectedRowBackgroundColor: "var(--selection)",
  fontFamily: "var(--font-mono)",
  headerFontFamily: "var(--font-sans)",
  fontSize: 12,
  rowHeight: 24,
  headerHeight: 28,
  wrapperBorderRadius: 0,
  borderRadius: 2,
});

/**
 * A VECTOR cell as the server sends it (float32 little-endian bytes, as `0x…` hex, or as text when
 * the bytes happen to be valid UTF-8) → `[1, 0.5, …]`. Anything else is returned unchanged.
 */
// ponytail: a UTF-8-valid vector whose first two bytes are "0x" would be misread as hex; vanishingly rare.
export function vectorText(v: string): string {
  const bytes = v.startsWith("0x") ? Uint8Array.from(v.slice(2).match(/../g) ?? [], (h) => parseInt(h, 16)) : new TextEncoder().encode(v);
  if (!bytes.length || bytes.length % 4) return v;
  const view = new DataView(bytes.buffer);
  const values = Array.from({ length: bytes.length / 4 }, (_, i) => +view.getFloat32(i * 4, true).toPrecision(7));
  return `[${values.join(", ")}]`;
}
