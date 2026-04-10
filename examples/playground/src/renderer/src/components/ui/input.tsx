import { forwardRef, type InputHTMLAttributes } from 'react';
import { cn } from '@/lib/cn';

export type InputProps = InputHTMLAttributes<HTMLInputElement>;

export const Input = forwardRef<HTMLInputElement, InputProps>(function Input(
  { className, type = 'text', ...rest },
  ref,
) {
  return (
    <input
      ref={ref}
      type={type}
      className={cn(
        'flex h-8 w-full rounded-md border border-[var(--color-border-subtle)] ' +
          'bg-[var(--color-surface)] px-2.5 text-[13px] text-[var(--color-text-primary)] ' +
          'placeholder:text-[var(--color-text-muted)] ' +
          'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)] ' +
          'focus-visible:ring-offset-2 focus-visible:ring-offset-[var(--color-bg)] ' +
          'focus-visible:border-[var(--color-accent)] ' +
          'disabled:cursor-not-allowed disabled:opacity-50',
        className,
      )}
      {...rest}
    />
  );
});
