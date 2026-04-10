import type { HTMLAttributes } from 'react';
import { cn } from '@/lib/cn';

export type BadgeVariant =
  | 'default'
  | 'success'
  | 'warning'
  | 'error'
  | 'info'
  | 'muted';

export interface BadgeProps extends HTMLAttributes<HTMLSpanElement> {
  variant?: BadgeVariant;
}

const variants: Record<BadgeVariant, string> = {
  default:
    'bg-[var(--color-accent)]/15 text-[var(--color-accent-hover)] ' +
    'border border-[var(--color-accent)]/30',
  success:
    'bg-[var(--color-status-green)]/15 text-[var(--color-status-green)] ' +
    'border border-[var(--color-status-green)]/30',
  warning:
    'bg-[var(--color-status-amber)]/15 text-[var(--color-status-amber)] ' +
    'border border-[var(--color-status-amber)]/30',
  error:
    'bg-[var(--color-status-red)]/15 text-[var(--color-status-red)] ' +
    'border border-[var(--color-status-red)]/30',
  info:
    'bg-[var(--color-status-blue)]/15 text-[var(--color-status-blue)] ' +
    'border border-[var(--color-status-blue)]/30',
  muted:
    'bg-[var(--color-surface-raised)] text-[var(--color-text-secondary)] ' +
    'border border-[var(--color-border-subtle)]',
};

export function Badge({
  className,
  variant = 'default',
  ...rest
}: BadgeProps) {
  return (
    <span
      className={cn(
        'inline-flex items-center gap-1 rounded px-1.5 py-0.5 ' +
          'text-[10.5px] font-semibold uppercase tracking-wide',
        variants[variant],
        className,
      )}
      {...rest}
    />
  );
}
