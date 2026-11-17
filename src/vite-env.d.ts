/// <reference types="vite/client" />

declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<{}, {}, any>;
  export default component;
}

// Monaco ships no types for its grammar files.
declare module "monaco-editor/languages/definitions/mysql/mysql" {
  export const language: { keywords: string[] };
}
