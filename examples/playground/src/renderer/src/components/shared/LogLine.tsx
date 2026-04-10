import { cn } from '@/lib/cn';
import { formatClockTime } from '@/lib/format';

export type LogLineVariant = 'info' | 'system' | 'sent' | 'received' | 'warn' | 'error';

interface LogLineProps {
  ts: number;
  text: string;
  who?: string;
  variant?: LogLineVariant;
  broadcast?: boolean;
}

const containerAlign: Record<LogLineVariant, string> = {
  info: 'justify-start',
  system: 'justify-center',
  sent: 'justify-end',
  received: 'justify-start',
  warn: 'justify-start',
  error: 'justify-start',
};

const bubbleTone: Record<LogLineVariant, string> = {
  info: 'bg-[var(--color-surface-raised)] text-[var(--color-text-primary)]',
  system:
    'bg-transparent text-[var(--color-text-muted)] italic text-[11px]',
  sent:
    'bg-[var(--color-accent)]/20 text-[var(--color-text-primary)] border border-[var(--color-accent)]/30',
  received:
    'bg-[var(--color-surface-raised)] text-[var(--color-text-primary)] border border-[var(--color-border-subtle)]',
  warn:
    'bg-[var(--color-status-amber)]/15 text-[var(--color-status-amber)] border border-[var(--color-status-amber)]/30',
  error:
    'bg-[var(--color-status-red)]/15 text-[var(--color-status-red)] border border-[var(--color-status-red)]/30',
};

export function LogLine({
  ts,
  text,
  who,
  variant = 'info',
  broadcast,
}: LogLineProps) {
  if (variant === 'system') {
    return (
      <div className="flex w-full items-center justify-center py-0.5">
        <span className="mono text-[10.5px] text-[var(--color-text-muted)]">
          {formatClockTime(ts)} — {text}
        </span>
      </div>
    );
  }
  return (
    <div className={cn('flex w-full items-start gap-2 py-0.5', containerAlign[variant])}>
      {variant !== 'sent' ? (
        <span className="mono shrink-0 pt-1 text-[10.5px] text-[var(--color-text-muted)]">
          {formatClockTime(ts)}
        </span>
      ) : null}
      <div
        className={cn(
          'max-w-[70%] rounded-md px-2.5 py-1 text-[13px] break-words selectable',
          bubbleTone[variant],
        )}
      >
        {who ? (
          <div className="mb-0.5 flex items-center gap-2 text-[10.5px] font-semibold uppercase tracking-wide text-[var(--color-text-secondary)]">
            <span>{who}</span>
            {broadcast ? (
              <span className="text-[var(--color-accent-hover)]">· broadcast</span>
            ) : null}
          </div>
        ) : null}
        <div>{text}</div>
      </div>
      {variant === 'sent' ? (
        <span className="mono shrink-0 pt-1 text-[10.5px] text-[var(--color-text-muted)]">
          {formatClockTime(ts)}
        </span>
      ) : null}
    </div>
  );
}
