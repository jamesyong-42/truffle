/**
 * Tiny inline class-name concatenator. Drop-in replacement for `clsx` for
 * the small surface area we need. Accepts strings, falsy values, and objects
 * with boolean values. Added to avoid a runtime dependency on `clsx`.
 */
export type ClassValue =
  | string
  | number
  | null
  | false
  | undefined
  | Record<string, boolean | null | undefined>
  | ClassValue[];

export function cn(...values: ClassValue[]): string {
  const out: string[] = [];
  for (const v of values) {
    if (!v) continue;
    if (typeof v === 'string' || typeof v === 'number') {
      out.push(String(v));
    } else if (Array.isArray(v)) {
      const nested = cn(...v);
      if (nested) out.push(nested);
    } else if (typeof v === 'object') {
      for (const key of Object.keys(v)) {
        if (v[key]) out.push(key);
      }
    }
  }
  return out.join(' ');
}
