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
