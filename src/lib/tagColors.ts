// Connection tag colours (e.g. red = production). Tailwind needs literal class names.
export const TAG_COLORS = {
  red: "bg-red-500",
  orange: "bg-orange-500",
  yellow: "bg-yellow-400",
  green: "bg-green-500",
  blue: "bg-blue-500",
  purple: "bg-purple-500",
} as const;

export type TagColor = keyof typeof TAG_COLORS;

export function tagClass(color: string | null | undefined): string {
  return (color && TAG_COLORS[color as TagColor]) || "bg-muted-foreground/40";
}
