import type { HealthInfo, NodeIdentity } from '@shared/ipc';
import { cn } from '@/lib/cn';

interface StatusBarProps {
  identity: NodeIdentity | null;
  health: HealthInfo | null;
  peerCount: number;
}

function healthColor(state?: string, healthy?: boolean): string {
  if (!state) return 'bg-[var(--color-text-muted)]';
  if (healthy === false) return 'bg-[var(--color-status-red)]';
  const s = state.toLowerCase();
  if (s.includes('running')) return 'bg-[var(--color-status-green)]';
  if (s.includes('starting') || s.includes('auth')) return 'bg-[var(--color-status-amber)]';
  if (s.includes('stop') || s.includes('error'))
    return 'bg-[var(--color-status-red)]';
  return 'bg-[var(--color-status-blue)]';
}

function daysUntil(iso?: string): number | null {
  if (!iso) return null;
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return null;
  const ms = t - Date.now();
  return Math.round(ms / (1000 * 60 * 60 * 24));
}

function Sep() {
  return (
    <span
      aria-hidden="true"
      className="mx-2 h-3 w-px bg-[var(--color-border-subtle)]"
    />
  );
}

export function StatusBar({ identity, health, peerCount }: StatusBarProps) {
  const expiryDays = daysUntil(health?.keyExpiry);
  const showExpiry = expiryDays !== null && expiryDays <= 7;
  return (
    <footer
      role="contentinfo"
      className="flex h-6 shrink-0 items-center border-t border-[var(--color-border-subtle)] bg-[var(--color-surface)] px-3 text-[11px] text-[var(--color-text-secondary)]"
    >
      <span
        className={cn(
          'mr-2 inline-block h-2 w-2 shrink-0 rounded-full',
          healthColor(health?.state, health?.healthy),
        )}
        aria-hidden="true"
      />
      <span className="text-[var(--color-text-primary)]">
        {identity?.name ?? 'starting…'}
      </span>
      {identity?.ip ? (
        <span className="mono ml-2 text-[var(--color-text-secondary)]">
          {identity.ip}
        </span>
      ) : null}
      <Sep />
      <span className="mono">{health?.state ?? 'unknown'}</span>
      <Sep />
      <span>
        {peerCount} {peerCount === 1 ? 'peer' : 'peers'}
      </span>
      {showExpiry && expiryDays !== null ? (
        <>
          <Sep />
          <span className="text-[var(--color-status-amber)]">
            key expires in {expiryDays}d
          </span>
        </>
      ) : null}
      <div className="flex-1" />
      {identity?.id ? (
        <span
          className="mono text-[10.5px] text-[var(--color-text-muted)]"
          title={identity.id}
        >
          {identity.id.slice(0, 12)}…
        </span>
      ) : null}
    </footer>
  );
}
