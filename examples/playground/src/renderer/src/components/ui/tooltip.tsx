import type { ReactNode } from 'react';
import { cn } from '@/lib/cn';

interface TooltipProps {
  label: string;
  side?: 'top' | 'right' | 'bottom' | 'left';
  children: ReactNode;
  className?: string;
}

/**
 * CSS-only hover tooltip. Wraps a child with a group span and reveals a
 * small label on hover or focus. Good enough for a dev tool; keeps us
 * dependency-free.
 */
export function Tooltip({
  label,
  side = 'right',
  children,
  className,
}: TooltipProps) {
  const sidePos: Record<string, string> = {
    top: 'bottom-full left-1/2 -translate-x-1/2 mb-1',
    bottom: 'top-full left-1/2 -translate-x-1/2 mt-1',
    left: 'right-full top-1/2 -translate-y-1/2 mr-1',
    right: 'left-full top-1/2 -translate-y-1/2 ml-1',
  };
  return (
    <span className={cn('relative inline-flex group', className)}>
      {children}
      <span
        role="tooltip"
        className={cn(
          'pointer-events-none absolute z-50 whitespace-nowrap rounded border ' +
            'border-[var(--color-border-subtle)] bg-[var(--color-surface-raised)] ' +
            'px-2 py-1 text-[11px] text-[var(--color-text-primary)] shadow-lg ' +
            'opacity-0 transition-opacity duration-150 ' +
            'group-hover:opacity-100 group-focus-within:opacity-100',
          sidePos[side],
        )}
      >
        {label}
      </span>
    </span>
  );
}
