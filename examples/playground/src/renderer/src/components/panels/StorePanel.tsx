import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
} from 'react';
import type { StoreSlice } from '@shared/ipc';
import { useStore } from '@/hooks/useStore';
import { usePeers } from '@/hooks/usePeers';
import { cn } from '@/lib/cn';
import { formatRelativeTime } from '@/lib/format';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { ScrollArea } from '@/components/ui/scroll-area';
import { StoreRow } from '@/components/shared/StoreRow';

export function StorePanel() {
  const { slices, localKv, localSlice, setKey, unsetKey } = useStore();
  const { peers } = usePeers();
  const [newKey, setNewKey] = useState('');
  const [newValue, setNewValue] = useState('');
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [flashIds, setFlashIds] = useState<Record<string, number>>({});
  const prevVersionsRef = useRef<Record<string, number>>({});

  const peerSlices = useMemo(
    () =>
      [...slices]
        .filter((s) => !s.isLocal)
        .sort((a, b) => b.updatedAt - a.updatedAt),
    [slices],
  );

  // Flash peer sections when their version increases.
  useEffect(() => {
    const prev = prevVersionsRef.current;
    const now = Date.now();
    const updates: Record<string, number> = {};
    for (const slice of peerSlices) {
      const old = prev[slice.deviceId];
      if (old !== undefined && slice.version > old) {
        updates[slice.deviceId] = now;
      }
      prev[slice.deviceId] = slice.version;
    }
    if (Object.keys(updates).length > 0) {
      setFlashIds((current) => ({ ...current, ...updates }));
      setTimeout(() => {
        setFlashIds((current) => {
          const next = { ...current };
          for (const id of Object.keys(updates)) {
            if (next[id] === updates[id]) delete next[id];
          }
          return next;
        });
      }, 1000);
    }
  }, [peerSlices]);

  const addKey = async (e: FormEvent) => {
    e.preventDefault();
    const k = newKey.trim();
    const v = newValue;
    if (!k) return;
    await setKey(k, v);
    setNewKey('');
    setNewValue('');
  };

  const peerName = (deviceId: string): string => {
    const match = peers.find((p) => p.deviceId === deviceId);
    return match?.deviceName ?? deviceId.slice(0, 10);
  };

  const localUpdatedAgo = localSlice
    ? formatRelativeTime(localSlice.updatedAt)
    : '—';
  const localVersion = localSlice?.version ?? 0;

  const localKvEntries = Object.entries(localKv);

  const toggleExpanded = (id: string) => {
    setExpanded((current) => ({ ...current, [id]: !current[id] }));
  };

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-center justify-between border-b border-[var(--color-border-subtle)] px-4 py-2">
        <h2 className="section-header">Synced Store</h2>
        <span className="text-[11px] text-[var(--color-text-muted)]">
          {slices.length} {slices.length === 1 ? 'slice' : 'slices'}
        </span>
      </header>

      <ScrollArea className="flex-1">
        {/* Local slice */}
        <section className="border-b border-[var(--color-border-subtle)] p-4">
          <div className="mb-2 flex items-baseline gap-3">
            <h3 className="section-header">Your slice</h3>
            <span className="mono text-[11px] text-[var(--color-status-purple)]">
              v{localVersion}
            </span>
            <span className="text-[11px] text-[var(--color-text-muted)]">
              · {localUpdatedAgo}
            </span>
          </div>

          <div className="rounded-md border border-[var(--color-border-subtle)] bg-[var(--color-surface)]">
            {localKvEntries.length === 0 ? (
              <div className="px-3 py-4 text-center text-[12px] text-[var(--color-text-muted)]">
                No keys set yet. Add one below.
              </div>
            ) : (
              localKvEntries
                .sort(([a], [b]) => a.localeCompare(b))
                .map(([k, v]) => (
                  <StoreRow
                    key={k}
                    k={k}
                    v={v}
                    editable
                    onSave={(key, val) => setKey(key, val)}
                    onDelete={(key) => unsetKey(key)}
                  />
                ))
            )}
          </div>

          <form
            onSubmit={(e) => void addKey(e)}
            className="mt-3 flex items-center gap-2"
          >
            <Input
              value={newKey}
              onChange={(e) => setNewKey(e.target.value)}
              placeholder="key"
              className="mono max-w-[200px]"
              aria-label="New key"
            />
            <Input
              value={newValue}
              onChange={(e) => setNewValue(e.target.value)}
              placeholder="value"
              className="mono"
              aria-label="New value"
            />
            <Button type="submit" disabled={!newKey.trim()}>
              Set
            </Button>
          </form>
        </section>

        {/* Peer slices */}
        <section className="p-4">
          <div className="mb-2 flex items-baseline gap-3">
            <h3 className="section-header">Peer slices</h3>
            <span className="text-[11px] text-[var(--color-text-muted)]">
              {peerSlices.length}
            </span>
          </div>

          {peerSlices.length === 0 ? (
            <div className="rounded-md border border-dashed border-[var(--color-border-subtle)] p-6 text-center text-[12px] text-[var(--color-text-muted)]">
              No peer slices yet. Peers who write to the SyncedStore will
              appear here.
            </div>
          ) : (
            <div className="flex flex-col gap-2">
              {peerSlices.map((slice) => (
                <PeerSliceSection
                  key={slice.deviceId}
                  slice={slice}
                  name={peerName(slice.deviceId)}
                  expanded={!!expanded[slice.deviceId]}
                  onToggle={() => toggleExpanded(slice.deviceId)}
                  flashing={!!flashIds[slice.deviceId]}
                />
              ))}
            </div>
          )}
        </section>
      </ScrollArea>
    </div>
  );
}

interface PeerSliceSectionProps {
  slice: StoreSlice;
  name: string;
  expanded: boolean;
  onToggle: () => void;
  flashing: boolean;
}

function PeerSliceSection({
  slice,
  name,
  expanded,
  onToggle,
  flashing,
}: PeerSliceSectionProps) {
  const entries = Object.entries(slice.data.kv).sort(([a], [b]) =>
    a.localeCompare(b),
  );
  return (
    <div
      className={cn(
        'overflow-hidden rounded-md border border-[var(--color-border-subtle)] bg-[var(--color-surface)]',
        flashing && 'flash-highlight',
      )}
    >
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={expanded}
        className="flex w-full items-center gap-3 px-3 py-2 text-left hover:bg-[var(--color-surface-raised)]/60"
      >
        <span className="mono text-[11px] text-[var(--color-text-muted)]">
          {expanded ? '▼' : '▶'}
        </span>
        <span className="flex-1 truncate text-[12.5px] text-[var(--color-text-primary)]">
          {name}
        </span>
        <span className="mono text-[11px] text-[var(--color-status-purple)]">
          v{slice.version}
        </span>
        <span className="text-[11px] text-[var(--color-text-muted)]">
          {formatRelativeTime(slice.updatedAt)}
        </span>
        <span className="text-[11px] text-[var(--color-text-muted)]">
          ({entries.length} {entries.length === 1 ? 'key' : 'keys'})
        </span>
      </button>
      {expanded ? (
        <div className="border-t border-[var(--color-border-subtle)]">
          {entries.length === 0 ? (
            <div className="px-3 py-2 text-[11px] text-[var(--color-text-muted)]">
              empty
            </div>
          ) : (
            entries.map(([k, v]) => <StoreRow key={k} k={k} v={v} />)
          )}
        </div>
      ) : null}
    </div>
  );
}
