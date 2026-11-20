import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

/** Merge Tailwind classes, later ones win (shadcn-vue convention). */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** `VERSION()` ("8.0.46", "5.7.44-log", "12.3.3-MariaDB-…") is at least `min`. */
export function versionAtLeast(version: string, min: [number, number, number]): boolean {
  const v = version.split(/\D+/).map(Number);
  for (let i = 0; i < 3; i++) if ((v[i] ?? 0) !== min[i]) return (v[i] ?? 0) > min[i];
  return true;
}
