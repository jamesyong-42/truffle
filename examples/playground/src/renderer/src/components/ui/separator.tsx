import type { HTMLAttributes } from 'react';
import { cn } from '@/lib/cn';

export interface SeparatorProps extends HTMLAttributes<HTMLDivElement> {
  orientation?: 'horizontal' | 'vertical';
}

export function Separator({
  orientation = 'horizontal',
  className,
  ...rest
}: SeparatorProps) {
  return (
    <div
      role="separator"
      aria-orientation={orientation}
      className={cn(
        'bg-[var(--color-border-subtle)]',
        orientation === 'horizontal' ? 'h-px w-full' : 'h-full w-px',
        className,
      )}
      {...rest}
    />
  );
}
