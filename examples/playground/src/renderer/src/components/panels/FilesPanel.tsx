import {
  useMemo,
  useState,
  type DragEvent,
} from 'react';
import type { Peer } from '@shared/ipc';
import { cn } from '@/lib/cn';
import { formatBytes } from '@/lib/format';
import { useFileTransfer } from '@/hooks/useFileTransfer';
import { usePeers } from '@/hooks/usePeers';
import { Button } from '@/components/ui/button';
import { ScrollArea } from '@/components/ui/scroll-area';
import { TransferRow } from '@/components/shared/TransferRow';
import { PeerBadge } from '@/components/shared/PeerBadge';

export function FilesPanel() {
  const { transfers, pendingOffers, sendFile, accept, reject } =
    useFileTransfer();
  const { peers } = usePeers();

  const [dropHighlight, setDropHighlight] = useState(false);
  const [pendingPath, setPendingPath] = useState<string | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);

  const connectedPeers = useMemo(
    () => peers.filter((p) => p.wsConnected),
    [peers],
  );

  const active = useMemo(
    () => transfers.filter((t) => t.status === 'active' || t.status === 'pending'),
    [transfers],
  );
  const completed = useMemo(
    () => transfers.filter((t) => t.status === 'completed' || t.status === 'failed'),
    [transfers],
  );

  const onDragEnter = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    setDropHighlight(true);
  };
  const onDragOver = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
  };
  const onDragLeave = (e: DragEvent<HTMLDivElement>) => {
    // Only clear when leaving the panel entirely (relatedTarget outside).
    if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
      setDropHighlight(false);
    }
  };

  const onDrop = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    setDropHighlight(false);
    setSendError(null);
    const file = e.dataTransfer.files?.[0];
    if (!file) return;
    // Electron extends File with `.path` — sanctioned by the task spec.
    const path = file.path;
    if (!path) {
      setSendError(
        'Dropped file has no filesystem path (Electron extension missing).',
      );
      return;
    }
    setPendingPath(path);
    setPickerOpen(true);
  };

  const openPicker = async () => {
    const picked = await window.truffle.pickFile();
    if (!picked) return;
    setSendError(null);
    setPendingPath(picked);
    setPickerOpen(true);
  };

  const confirmSend = async (peer: Peer) => {
    if (!pendingPath) return;
    setPickerOpen(false);
    const path = pendingPath;
    setPendingPath(null);
    try {
      await sendFile(peer.deviceId, path);
    } catch (err) {
      setSendError(err instanceof Error ? err.message : String(err));
    }
  };

  const acceptOffer = async (token: string, fileName: string) => {
    const downloads = await window.truffle.getDownloadsDir();
    const savePath = `${downloads}/${fileName}`;
    await accept(token, savePath);
  };
  const acceptOfferSaveAs = async (token: string, fileName: string) => {
    const savePath = await window.truffle.pickSavePath(fileName);
    if (!savePath) return;
    await accept(token, savePath);
  };

  return (
    <div
      className={cn(
        'relative flex h-full flex-col',
        dropHighlight && 'ring-2 ring-[var(--color-accent)] ring-inset',
      )}
      onDragEnter={onDragEnter}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
    >
      <header className="flex items-center justify-between border-b border-[var(--color-border-subtle)] px-4 py-2">
        <h2 className="section-header">File Transfer</h2>
        <Button size="sm" onClick={() => void openPicker()}>
          Send File…
        </Button>
      </header>

      <ScrollArea className="flex-1 p-4">
        {/* Drop zone */}
        <section
          className={cn(
            'mb-4 flex flex-col items-center justify-center rounded-lg border-2 border-dashed border-[var(--color-border-subtle)] bg-[var(--color-surface)] px-6 py-8 transition-all',
            dropHighlight &&
              'border-[var(--color-accent)] bg-[var(--color-accent)]/10',
          )}
          aria-label="File drop zone"
        >
          <svg
            width="28"
            height="28"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.6"
            strokeLinecap="round"
            strokeLinejoin="round"
            className="mb-2 text-[var(--color-accent-hover)]"
            aria-hidden="true"
          >
            <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
            <polyline points="17 8 12 3 7 8" />
            <line x1="12" y1="3" x2="12" y2="15" />
          </svg>
          <p className="text-[13px] text-[var(--color-text-primary)]">
            Drag files here to send to a peer
          </p>
          <p className="mt-1 text-[11px] text-[var(--color-text-muted)]">
            — or —
          </p>
          <Button
            variant="secondary"
            size="sm"
            className="mt-2"
            onClick={() => void openPicker()}
          >
            Send File…
          </Button>
          {sendError ? (
            <p className="mt-3 text-[11px] text-[var(--color-status-red)]">
              {sendError}
            </p>
          ) : null}
        </section>

        {/* Pending offers */}
        {pendingOffers.length > 0 ? (
          <section className="mb-4">
            <h3 className="section-header mb-2">Pending offers</h3>
            <div className="flex flex-col gap-2">
              {pendingOffers.map((offer) => (
                <div
                  key={offer.token}
                  className="flex items-center justify-between gap-3 rounded-md border border-[var(--color-status-blue)]/40 bg-[var(--color-status-blue)]/10 px-3 py-2"
                >
                  <div className="flex min-w-0 items-center gap-3">
                    <span
                      aria-hidden="true"
                      className="text-[16px] text-[var(--color-status-blue)]"
                    >
                      ↓
                    </span>
                    <div className="min-w-0">
                      <div className="mono truncate text-[12.5px] text-[var(--color-text-primary)]">
                        {offer.fileName}
                      </div>
                      <div className="mono text-[10.5px] text-[var(--color-text-muted)]">
                        {formatBytes(offer.size)} · from {offer.fromName}
                      </div>
                    </div>
                  </div>
                  <div className="flex items-center gap-1.5">
                    <Button
                      size="sm"
                      onClick={() =>
                        void acceptOffer(offer.token, offer.fileName)
                      }
                    >
                      Accept
                    </Button>
                    <Button
                      size="sm"
                      variant="secondary"
                      onClick={() =>
                        void acceptOfferSaveAs(offer.token, offer.fileName)
                      }
                    >
                      Save to…
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => void reject(offer.token)}
                    >
                      Reject
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          </section>
        ) : null}

        {/* Active transfers */}
        <section className="mb-4">
          <h3 className="section-header mb-2">Active transfers</h3>
          {active.length === 0 ? (
            <div className="rounded-md border border-dashed border-[var(--color-border-subtle)] p-4 text-center text-[11px] text-[var(--color-text-muted)]">
              No active transfers.
            </div>
          ) : (
            <div className="rounded-md border border-[var(--color-border-subtle)] bg-[var(--color-surface)]">
              {active.map((t) => (
                <TransferRow key={t.token} transfer={t} />
              ))}
            </div>
          )}
        </section>

        {/* Completed */}
        <section>
          <h3 className="section-header mb-2">Completed</h3>
          {completed.length === 0 ? (
            <div className="rounded-md border border-dashed border-[var(--color-border-subtle)] p-4 text-center text-[11px] text-[var(--color-text-muted)]">
              No completed transfers.
            </div>
          ) : (
            <div className="rounded-md border border-[var(--color-border-subtle)] bg-[var(--color-surface)]">
              {completed.map((t) => (
                <TransferRow key={t.token} transfer={t} />
              ))}
            </div>
          )}
        </section>
      </ScrollArea>

      {/* Send peer picker overlay */}
      {pickerOpen ? (
        <div
          role="dialog"
          aria-modal="true"
          aria-label="Choose peer to send to"
          className="absolute inset-0 z-40 flex items-center justify-center bg-black/60"
          onClick={() => {
            setPickerOpen(false);
            setPendingPath(null);
          }}
        >
          <div
            className="w-full max-w-sm rounded-lg border border-[var(--color-border-subtle)] bg-[var(--color-surface)] p-4 shadow-2xl"
            onClick={(e) => e.stopPropagation()}
          >
            <h3 className="mb-2 text-[13px] font-semibold text-[var(--color-text-primary)]">
              Send to which peer?
            </h3>
            {pendingPath ? (
              <p className="mono mb-3 truncate text-[11px] text-[var(--color-text-code)]">
                {pendingPath}
              </p>
            ) : null}
            {connectedPeers.length === 0 ? (
              <p className="text-[12px] text-[var(--color-text-muted)]">
                No connected peers. Wait for a peer to come online and try
                again.
              </p>
            ) : (
              <ul className="flex flex-col gap-1">
                {connectedPeers.map((peer) => (
                  <li key={peer.deviceId}>
                    <button
                      type="button"
                      onClick={() => void confirmSend(peer)}
                      className="flex w-full items-center justify-between rounded-md border border-transparent px-2 py-1.5 text-left hover:border-[var(--color-accent)]/40 hover:bg-[var(--color-surface-raised)]"
                    >
                      <PeerBadge peer={peer} />
                      <span className="mono text-[10.5px] text-[var(--color-text-muted)]">
                        {peer.ip}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
            <div className="mt-3 flex justify-end">
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  setPickerOpen(false);
                  setPendingPath(null);
                }}
              >
                Cancel
              </Button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
