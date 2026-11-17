// Monaco, bundled locally (no CDN), with only the SQL language and the editor features we use.
import * as monaco from "monaco-editor/editor/editor.api";
import "monaco-editor/editor/contrib/bracketMatching/browser/bracketMatching";
import "monaco-editor/editor/contrib/clipboard/browser/clipboard";
import "monaco-editor/editor/contrib/comment/browser/comment";
import "monaco-editor/editor/contrib/contextmenu/browser/contextmenu";
import "monaco-editor/editor/contrib/cursorUndo/browser/cursorUndo";
import "monaco-editor/editor/contrib/find/browser/findController";
import "monaco-editor/editor/contrib/hover/browser/hoverContribution";
import "monaco-editor/editor/contrib/linesOperations/browser/linesOperations";
import "monaco-editor/editor/contrib/multicursor/browser/multicursor";
import "monaco-editor/editor/contrib/suggest/browser/suggestController";
import "monaco-editor/editor/contrib/wordHighlighter/browser/wordHighlighter";
import "monaco-editor/editor/contrib/wordOperations/browser/wordOperations";
import "monaco-editor/features/codicon/register";
import "monaco-editor/languages/definitions/mysql/register";
import { language as mysql } from "monaco-editor/languages/definitions/mysql/mysql";
import EditorWorker from "monaco-editor/editor/editor.worker?worker";

self.MonacoEnvironment = { getWorker: () => new EditorWorker() };

// ponytail: Monaco's MySQL grammar, close enough for MariaDB; add MariaDB-only keywords if highlighting misses some.
const KEYWORDS = mysql.keywords;

/** Theme colours come from tokens.css, re-read when the app switches light/dark. */
function applyTheme() {
  const css = getComputedStyle(document.documentElement);
  const v = (name: string) => css.getPropertyValue(name).trim();
  const dark = document.documentElement.classList.contains("dark");
  monaco.editor.defineTheme("otaria", {
    base: dark ? "vs-dark" : "vs",
    inherit: true,
    rules: [
      { token: "keyword", foreground: v("--primary") },
      { token: "string", foreground: v("--success") },
      { token: "number", foreground: v("--warning") },
      { token: "comment", foreground: v("--muted-foreground"), fontStyle: "italic" },
      { token: "operator", foreground: v("--foreground") },
    ],
    colors: {
      "editor.background": v("--background"),
      "editor.foreground": v("--foreground"),
      "editor.selectionBackground": v("--selection"),
      "editor.lineHighlightBackground": v("--card"),
      "editorLineNumber.foreground": v("--muted-foreground"),
      "editorWidget.background": v("--popover"),
      "editorWidget.border": v("--border"),
      "editorSuggestWidget.background": v("--popover"),
      "editorSuggestWidget.selectedBackground": v("--selection"),
    },
  });
  monaco.editor.setTheme("otaria");
}
applyTheme();
new MutationObserver(applyTheme).observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });

/** Metadata an editor tab provides for completion; loaded on demand and cached by the tab itself. */
export interface CompletionSource {
  schema: () => string | null;
  schemas: () => Promise<string[]>;
  tables: (schema: string) => Promise<string[]>;
  columns: (schema: string, table: string) => Promise<string[]>;
}
export const sources = new WeakMap<monaco.editor.ITextModel, CompletionSource>();

const unquote = (s: string) => s.replace(/`/g, "");
const ID = "`[^`]+`|\\w+";

monaco.languages.registerCompletionItemProvider("mysql", {
  triggerCharacters: ["."],
  async provideCompletionItems(model, pos) {
    const src = sources.get(model);
    const word = model.getWordUntilPosition(pos);
    const range = new monaco.Range(pos.lineNumber, word.startColumn, pos.lineNumber, word.endColumn);
    const K = monaco.languages.CompletionItemKind;
    const item = (label: string, kind: monaco.languages.CompletionItemKind, detail?: string) => ({ label, kind, detail, insertText: label, range });
    if (!src) return { suggestions: [] };
    const before = model.getLineContent(pos.lineNumber).slice(0, word.startColumn - 1);
    const schemas = await src.schemas();
    const current = src.schema();

    // After "x.": tables of schema x, or columns of table x.
    const dot = before.match(new RegExp(`(?:(${ID})\\.)?(${ID})\\.$`));
    if (dot) {
      const [, a, b] = dot.map((s) => (s ? unquote(s) : s));
      if (!a && schemas.includes(b)) return { suggestions: (await src.tables(b)).map((t) => item(t, K.Struct, b)) };
      const schema = a ?? current;
      return { suggestions: schema ? (await src.columns(schema, b)).map((c) => item(c, K.Field, b)) : [] };
    }

    // Columns of the tables named in the script, then tables, schemas and keywords.
    const named = [...model.getValue().matchAll(new RegExp(`\\b(?:from|join|update|into)\\s+(?:(${ID})\\.)?(${ID})`, "gi"))];
    const cols = await Promise.all(
      named.map(async ([, s, tb]) => {
        const schema = s ? unquote(s) : current;
        return schema ? (await src.columns(schema, unquote(tb)).catch(() => [])).map((c) => item(c, K.Field, unquote(tb))) : [];
      }),
    );
    const tables = current ? (await src.tables(current)).map((t) => item(t, K.Struct, current)) : [];
    const all = [...cols.flat(), ...tables, ...schemas.map((s) => item(s, K.Module)), ...KEYWORDS.map((k) => item(k, K.Keyword))];
    return { suggestions: [...new Map(all.map((x) => [x.label, x])).values()] };
  },
});

export { monaco };
