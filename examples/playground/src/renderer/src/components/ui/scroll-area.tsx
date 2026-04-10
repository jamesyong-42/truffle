import { forwardRef, type HTMLAttributes } from 'react';
import { cn } from '@/lib/cn';

export type ScrollAreaProps = HTMLAttributes<HTMLDivElement>;

/**
 * Minimal scroll container. Not backed by Radix — just a div with
 * `overflow-y-auto` and our custom scrollbar styles.
 */
export const ScrollArea = forwardRef<HTMLDivElement, ScrollAreaProps>(
  function ScrollArea({ className, ...rest }, ref) {
    return (
      <div
        ref={ref}
        className={cn('overflow-y-auto overflow-x-hidden', className)}
        {...rest}
      />
    );
  },
);
