import { useMemo, useState } from 'react';
import type { Peer, PingResult } from '@shared/ipc';
import { cn } from '@/lib/cn';
import { usePeers } from '@/hooks/usePeers';
import { useHealth } from '@/hooks/useHealth';
import { Button } from '@/components/ui/button';
import { Badge } from '@/components/ui/badge';
import { ScrollArea } from '@/components/ui/scroll-area';
import { PeerBadge } from '@/components/shared/PeerBadge';

function peerSortKey(p: Peer): number {
  if (p.wsConnected) return 0;
  if (p.online) return 1;
  return 2;
}

function connectionLabel(peer: Peer): string {
  if (peer.wsConnected) return `ws · ${peer.connectionType || 'direct'}`;
  if (peer.online) return peer.connectionType || 'tailscale';
  return 'offline';
}

export function PeersPanel() {
  const { peers, refresh } = usePeers();
  const { ping } = useHealth();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [pingResult, setPingResult] = useState<PingResult | null>(null);
  const [pingError, setPingError] = useState<string | null>(null);
  const [pinging, setPinging] = useState(false);

  const sorted = useMemo(
    () =>
      [...peers].sort((a, b) => {
        const sa = peerSortKey(a);
        const sb = peerSortKey(b);
        if (sa !== sb) return sa - sb;
        return a.name.localeCompare(b.name);
      }),
    [peers],
  );

  const selected = sorted.find((p) => p.id === selectedId) ?? null;

  const handlePing = async () => {
    if (!selected) return;
    setPingResult(null);
    setPingError(null);
    setPinging(true);
    try {
      const result = await ping(selected.id);
      setPingResult(result);
    } catch (err) {
      setPingError(err instanceof Error ? err.message : String(err));
    } finally {
      setPinging(false);
    }
  };

  const select = (peer: Peer) => {
    setSelectedId(peer.id);
    setPingResult(null);
    setPingError(null);
  };

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-center justify-between border-b border-[var(--color-border-subtle)] px-4 py-2">
        <div className="flex items-baseline gap-3">
          <h2 className="section-header">Peers</h2>
          <span className="text-[11px] text-[var(--color-text-secondary)]">
            {sorted.length} {sorted.length === 1 ? 'peer' : 'peers'}
          </span>
        </div>
        <Button variant="secondary" size="sm" onClick={() => void refresh()}>
          Refresh
        </Button>
      </header>

      <ScrollArea className="flex-1">
        {sorted.length === 0 ? (
          <div className="flex h-full items-center justify-center p-10 text-center text-[12px] text-[var(--color-text-muted)]">
            No peers discovered yet. Waiting for tailnet updates…
          </div>
        ) : (
          <ul className="divide-y divide-[var(--color-border-subtle)]">
            {sorted.map((peer) => {
              const isSelected = peer.id === selectedId;
              return (
                <li key={peer.id}>
                  <button
                    type="button"
                    onClick={() => select(peer)}
                    aria-current={isSelected ? 'true' : undefined}
                    className={cn(
                      'grid w-full grid-cols-[minmax(0,1.4fr)_minmax(0,1fr)_minmax(0,1.2fr)] items-center gap-3 px-4 py-2 text-left transition-colors',
                      'hover:bg-[var(--color-surface-raised)]/60',
                      isSelected && 'bg-[var(--color-surface-raised)]',
                    )}
                  >
                    <PeerBadge peer={peer} />
                    <span className="mono truncate text-[11.5px] text-[var(--color-text-secondary)]">
                      {peer.ip || '—'}
                    </span>
                    <span className="mono truncate text-[11.5px] text-[var(--color-text-muted)]">
                      {connectionLabel(peer)}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </ScrollArea>

      {selected ? (
        <section
          className="border-t border-[var(--color-border-subtle)] bg-[var(--color-surface)] p-4"
          aria-label={`Peer details: ${selected.name}`}
        >
          <div className="mb-3 flex items-center justify-between">
            <div className="flex items-baseline gap-2">
              <h3 className="text-[13px] font-semibold text-[var(--color-text-primary)]">
                {selected.name}
              </h3>
              <Badge
                variant={
                  selected.wsConnected
                    ? 'success'
                    : selected.online
                      ? 'warning'
                      : 'error'
                }
              >
                {connectionLabel(selected)}
              </Badge>
            </div>
            <div className="flex items-center gap-2">
              <Button
                size="sm"
                onClick={() => void handlePing()}
                disabled={pinging}
              >
                {pinging ? 'Pinging…' : 'Ping'}
              </Button>
            </div>
          </div>
          <dl className="grid grid-cols-[90px_minmax(0,1fr)] gap-x-3 gap-y-1.5 text-[11.5px]">
            <dt className="text-[var(--color-text-muted)]">ID</dt>
            <dd className="mono truncate text-[var(--color-text-code)] selectable">
              {selected.id}
            </dd>
            <dt className="text-[var(--color-text-muted)]">IP</dt>
            <dd className="mono text-[var(--color-text-code)]">
              {selected.ip || '—'}
            </dd>
            <dt className="text-[var(--color-text-muted)]">OS</dt>
            <dd className="text-[var(--color-text-secondary)]">
              {selected.os ?? 'unknown'}
            </dd>
            <dt className="text-[var(--color-text-muted)]">Connection</dt>
            <dd className="text-[var(--color-text-secondary)]">
              {selected.connectionType || 'unknown'}
            </dd>
            <dt className="text-[var(--color-text-muted)]">Last seen</dt>
            <dd className="text-[var(--color-text-secondary)]">
              {selected.lastSeen ?? 'now'}
            </dd>
            <dt className="text-[var(--color-text-muted)]">Ping</dt>
            <dd className="text-[var(--color-text-secondary)]">
              {pingError ? (
                <span className="text-[var(--color-status-red)]">
                  {pingError}
                </span>
              ) : pingResult ? (
                <span className="mono">
                  {pingResult.latencyMs.toFixed(1)}ms · {pingResult.connection}
                </span>
              ) : (
                <span className="text-[var(--color-text-muted)]">
                  click Ping to measure
                </span>
              )}
            </dd>
          </dl>
        </section>
      ) : null}
    </div>
  );
}
