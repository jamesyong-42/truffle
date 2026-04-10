import { forwardRef, type ButtonHTMLAttributes } from 'react';
import { cn } from '@/lib/cn';

export type ButtonVariant =
  | 'default'
  | 'secondary'
  | 'ghost'
  | 'destructive'
  | 'outline';
export type ButtonSize = 'sm' | 'md' | 'lg' | 'icon';

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
}

const base =
  'inline-flex items-center justify-center gap-1.5 rounded-md font-medium ' +
  'transition-colors disabled:pointer-events-none disabled:opacity-50 ' +
  'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)] ' +
  'focus-visible:ring-offset-2 focus-visible:ring-offset-[var(--color-bg)] ' +
  'select-none whitespace-nowrap';

const variants: Record<ButtonVariant, string> = {
  default:
    'bg-[var(--color-accent)] text-white hover:bg-[var(--color-accent-hover)]',
  secondary:
    'bg-[var(--color-surface-raised)] text-[var(--color-text-primary)] ' +
    'hover:bg-[#2a3050] border border-[var(--color-border-subtle)]',
  ghost:
    'bg-transparent text-[var(--color-text-secondary)] ' +
    'hover:bg-[var(--color-surface-raised)] hover:text-[var(--color-text-primary)]',
  destructive:
    'bg-[var(--color-status-red)] text-white hover:bg-red-400',
  outline:
    'bg-transparent text-[var(--color-text-primary)] ' +
    'border border-[var(--color-border-subtle)] hover:bg-[var(--color-surface-raised)]',
};

const sizes: Record<ButtonSize, string> = {
  sm: 'h-7 px-2.5 text-[12px]',
  md: 'h-8 px-3 text-[13px]',
  lg: 'h-10 px-4 text-[14px]',
  icon: 'h-8 w-8 p-0',
};

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(
  function Button(
    { className, variant = 'default', size = 'md', type = 'button', ...rest },
    ref,
  ) {
    return (
      <button
        ref={ref}
        type={type}
        className={cn(base, variants[variant], sizes[size], className)}
        {...rest}
      />
    );
  },
);
