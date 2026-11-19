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
import { namedTables } from "@/lib/search";
import EditorWorker from "monaco-editor/editor/editor.worker?worker";

self.MonacoEnvironment = { getWorker: () => new EditorWorker() };

// Monaco's MySQL grammar plus MariaDB's own words; mutated before the first editor compiles it.
mysql.keywords.push(
  ..."RETURNING SYSTEM_TIME VERSIONING HISTORY PERIOD OVERLAPS SEQUENCE NEXTVAL LASTVAL SETVAL INCREMENT MINVALUE NOMINVALUE NOMAXVALUE NOCACHE NOCYCLE CYCLE RESTART VECTOR INVISIBLE IGNORED PACKAGE ROWNUM".split(" "),
);
mysql.builtinFunctions.push(..."VEC_DISTANCE VEC_DISTANCE_COSINE VEC_DISTANCE_EUCLIDEAN VEC_FROMTEXT VEC_TOTEXT JSON_TABLE JSON_DETAILED JSON_NORMALIZE JSON_EQUALS SFORMAT NATURAL_SORT_KEY".split(" "));
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

    const named = namedTables(model.getValue());

    // After "x.": columns of the table aliased x, tables of schema x, or columns of table x.
    const dot = before.match(new RegExp(`(?:(${ID})\\.)?(${ID})\\.$`));
    if (dot) {
      const [, a, b] = dot.map((s) => (s ? unquote(s) : s));
      const aliased = !a && named.find((n) => n.alias === b.toLowerCase());
      if (aliased) {
        const schema = aliased.schema ?? current;
        return { suggestions: schema ? (await src.columns(schema, aliased.table)).map((c) => item(c, K.Field, aliased.table)) : [] };
      }
      if (!a && schemas.includes(b)) return { suggestions: (await src.tables(b)).map((t) => item(t, K.Struct, b)) };
      const schema = a ?? current;
      return { suggestions: schema ? (await src.columns(schema, b)).map((c) => item(c, K.Field, b)) : [] };
    }

    // Columns of the tables named in the script, then tables, schemas and keywords.
    const cols = await Promise.all(
      named.map(async ({ schema: s, table }) => {
        const schema = s ?? current;
        return schema ? (await src.columns(schema, table).catch(() => [])).map((c) => item(c, K.Field, table)) : [];
      }),
    );
    const tables = current ? (await src.tables(current)).map((t) => item(t, K.Struct, current)) : [];
    const all = [...cols.flat(), ...tables, ...schemas.map((s) => item(s, K.Module)), ...KEYWORDS.map((k) => item(k, K.Keyword))];
    return { suggestions: [...new Map(all.map((x) => [x.label, x])).values()] };
  },
});

export { monaco };
