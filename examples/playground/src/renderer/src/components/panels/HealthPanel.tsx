import { useMemo, useState } from 'react';
import type { Peer, PingResult } from '@shared/ipc';
import { cn } from '@/lib/cn';
import { useHealth } from '@/hooks/useHealth';
import { usePeers } from '@/hooks/usePeers';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import { ScrollArea } from '@/components/ui/scroll-area';

interface PingCellState {
  result?: PingResult;
  error?: string;
  pinging?: boolean;
}

function latencyBar(latencyMs?: number): {
  filled: number;
  tone: 'green' | 'amber' | 'red' | 'muted';
} {
  if (latencyMs === undefined)
    return { filled: 0, tone: 'muted' };
  if (latencyMs < 20) return { filled: 3, tone: 'green' };
  if (latencyMs < 100) return { filled: 2, tone: 'amber' };
  return { filled: 1, tone: 'red' };
}

const barTones = {
  green: 'bg-[var(--color-status-green)]',
  amber: 'bg-[var(--color-status-amber)]',
  red: 'bg-[var(--color-status-red)]',
  muted: 'bg-[var(--color-border-subtle)]',
};

export function HealthPanel() {
  const { health, ping } = useHealth();
  const { peers } = usePeers();
  const [pings, setPings] = useState<Record<string, PingCellState>>({});

  const sortedPeers = useMemo(
    () =>
      [...peers].sort((a, b) => {
        if (a.wsConnected !== b.wsConnected) return a.wsConnected ? -1 : 1;
        if (a.online !== b.online) return a.online ? -1 : 1;
        return a.name.localeCompare(b.name);
      }),
    [peers],
  );

  const doPing = async (peer: Peer) => {
    setPings((current) => ({
      ...current,
      [peer.id]: { ...current[peer.id], pinging: true, error: undefined },
    }));
    try {
      const result = await ping(peer.id);
      setPings((current) => ({
        ...current,
        [peer.id]: { result, pinging: false },
      }));
    } catch (err) {
      setPings((current) => ({
        ...current,
        [peer.id]: {
          error: err instanceof Error ? err.message : String(err),
          pinging: false,
        },
      }));
    }
  };

  const pingAll = async () => {
    const targets = sortedPeers.filter((p) => p.wsConnected);
    await Promise.all(targets.map((p) => doPing(p)));
  };

  const healthTone = !health
    ? 'muted'
    : health.healthy
      ? 'success'
      : 'warning';

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-center justify-between border-b border-[var(--color-border-subtle)] px-4 py-2">
        <h2 className="section-header">Network Health</h2>
        <Button size="sm" onClick={() => void pingAll()}>
          Ping all
        </Button>
      </header>

      <ScrollArea className="flex-1 p-4">
        {/* Health card */}
        <section className="mb-4 rounded-md border border-[var(--color-border-subtle)] bg-[var(--color-surface)] p-4">
          <div className="mb-3 flex items-center gap-3">
            <Badge
              variant={
                healthTone === 'success'
                  ? 'success'
                  : healthTone === 'warning'
                    ? 'warning'
                    : 'muted'
              }
            >
              {health?.state ?? 'unknown'}
            </Badge>
            {health?.keyExpiry ? (
              <span className="text-[11px] text-[var(--color-text-secondary)]">
                Key expires:{' '}
                <span className="mono text-[var(--color-text-code)]">
                  {health.keyExpiry}
                </span>
              </span>
            ) : null}
          </div>
          {health && health.warnings.length > 0 ? (
            <div className="flex flex-col gap-1.5">
              {health.warnings.map((w, idx) => (
                <div
                  key={`${idx}-${w}`}
                  className="rounded border border-[var(--color-status-amber)]/30 bg-[var(--color-status-amber)]/10 px-2 py-1 text-[11.5px] text-[var(--color-status-amber)]"
                >
                  {w}
                </div>
              ))}
            </div>
          ) : (
            <p className="text-[11.5px] text-[var(--color-text-muted)]">
              No warnings.
            </p>
          )}
        </section>

        {/* Latency table */}
        <section>
          <h3 className="section-header mb-2">Latency</h3>
          {sortedPeers.length === 0 ? (
            <div className="rounded-md border border-dashed border-[var(--color-border-subtle)] p-6 text-center text-[11.5px] text-[var(--color-text-muted)]">
              No peers to ping.
            </div>
          ) : (
            <div className="overflow-hidden rounded-md border border-[var(--color-border-subtle)]">
              <div className="grid grid-cols-[minmax(0,1.5fr)_minmax(0,1.3fr)_minmax(0,1.3fr)_72px] items-center gap-3 border-b border-[var(--color-border-subtle)] bg-[var(--color-surface-raised)] px-3 py-1.5 text-[10.5px] font-semibold uppercase tracking-wide text-[var(--color-text-secondary)]">
                <span>Name</span>
                <span>Latency</span>
                <span>Connection</span>
                <span className="text-right">Action</span>
              </div>
              {sortedPeers.map((peer) => {
                const cell = pings[peer.id];
                const latency = cell?.result?.latencyMs;
                const { filled, tone } = latencyBar(latency);
                return (
                  <div
                    key={peer.id}
                    className="grid grid-cols-[minmax(0,1.5fr)_minmax(0,1.3fr)_minmax(0,1.3fr)_72px] items-center gap-3 border-b border-[var(--color-border-subtle)] px-3 py-2 last:border-b-0"
                  >
                    <div className="flex min-w-0 items-center gap-2">
                      <span
                        aria-hidden="true"
                        className={cn(
                          'h-2 w-2 shrink-0 rounded-full',
                          peer.wsConnected
                            ? 'bg-[var(--color-status-green)]'
                            : peer.online
                              ? 'bg-[var(--color-status-amber)]'
                              : 'bg-[var(--color-status-red)]',
                        )}
                      />
                      <span className="truncate text-[12.5px] text-[var(--color-text-primary)]">
                        {peer.name}
                      </span>
                    </div>
                    <div className="flex items-center gap-2">
                      <span className="mono min-w-[48px] text-[11.5px] text-[var(--color-text-primary)]">
                        {cell?.pinging
                          ? '…'
                          : latency !== undefined
                            ? `${latency.toFixed(1)}ms`
                            : '—'}
                      </span>
                      <div className="flex items-center gap-0.5">
                        {[0, 1, 2].map((i) => (
                          <span
                            key={i}
                            className={cn(
                              'h-2 w-2 rounded-sm',
                              i < filled
                                ? barTones[tone]
                                : barTones.muted,
                            )}
                          />
                        ))}
                      </div>
                    </div>
                    <span className="mono truncate text-[11px] text-[var(--color-text-secondary)]">
                      {peer.wsConnected
                        ? peer.connectionType || 'direct'
                        : peer.online
                          ? 'not ws'
                          : 'offline'}
                    </span>
                    <div className="flex justify-end">
                      <Button
                        size="sm"
                        variant="secondary"
                        disabled={cell?.pinging}
                        onClick={() => void doPing(peer)}
                      >
                        {cell?.pinging ? '…' : 'Ping'}
                      </Button>
                    </div>
                    {cell?.error ? (
                      <div className="col-span-4 mt-1 text-[10.5px] text-[var(--color-status-red)]">
                        {cell.error}
                      </div>
                    ) : null}
                  </div>
                );
              })}
            </div>
          )}
        </section>
      </ScrollArea>
    </div>
  );
}
