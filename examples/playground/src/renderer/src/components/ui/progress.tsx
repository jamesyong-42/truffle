import type { HTMLAttributes } from 'react';
import { cn } from '@/lib/cn';

export interface ProgressProps extends HTMLAttributes<HTMLDivElement> {
  /** 0 – 100 */
  value: number;
  tone?: 'accent' | 'success' | 'error' | 'info';
}

const tones: Record<NonNullable<ProgressProps['tone']>, string> = {
  accent: 'bg-[var(--color-accent)]',
  success: 'bg-[var(--color-status-green)]',
  error: 'bg-[var(--color-status-red)]',
  info: 'bg-[var(--color-status-blue)]',
};

export function Progress({
  value,
  tone = 'info',
  className,
  ...rest
}: ProgressProps) {
  const pct = Math.max(0, Math.min(100, value));
  return (
    <div
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(pct)}
      className={cn(
        'h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-border-subtle)]',
        className,
      )}
      {...rest}
    >
      <div
        className={cn('h-full transition-[width] duration-200', tones[tone])}
        style={{ width: `${pct}%` }}
      />
    </div>
  );
}
