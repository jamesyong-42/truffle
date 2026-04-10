import { cn } from '@/lib/cn';
import { Progress } from '@/components/ui/progress';
import {
  formatBytes,
  formatElapsed,
  formatSpeed,
  shortHash,
} from '@/lib/format';
import type { TransferState } from '@/hooks/useFileTransfer';

interface TransferRowProps {
  transfer: TransferState;
}

function DirectionArrow({ dir }: { dir: 'send' | 'receive' }) {
  return (
    <span
      aria-hidden="true"
      className={cn(
        'mono inline-block w-3 text-[14px] leading-none',
        dir === 'send'
          ? 'text-[var(--color-status-blue)]'
          : 'text-[var(--color-status-green)]',
      )}
    >
      {dir === 'send' ? '↑' : '↓'}
    </span>
  );
}

function StatusIcon({ status }: { status: TransferState['status'] }) {
  if (status === 'completed') {
    return (
      <span
        className="text-[var(--color-status-green)]"
        aria-label="Completed"
      >
        ✓
      </span>
    );
  }
  if (status === 'failed') {
    return (
      <span className="text-[var(--color-status-red)]" aria-label="Failed">
        ✗
      </span>
    );
  }
  return null;
}

export function TransferRow({ transfer }: TransferRowProps) {
  const { status, totalBytes, bytesTransferred, speedBps } = transfer;
  const pct =
    totalBytes && totalBytes > 0
      ? (bytesTransferred / totalBytes) * 100
      : 0;

  return (
    <div className="grid grid-cols-[24px_minmax(0,2fr)_minmax(0,1fr)_minmax(0,2fr)_minmax(0,1.5fr)] items-center gap-3 border-b border-[var(--color-border-subtle)] px-3 py-2 last:border-b-0">
      <div className="flex items-center justify-center">
        {status === 'active' || status === 'pending' ? (
          <DirectionArrow dir={transfer.direction} />
        ) : (
          <StatusIcon status={status} />
        )}
      </div>

      <div className="min-w-0">
        <div className="mono truncate text-[12px] text-[var(--color-text-primary)]">
          {transfer.fileName}
        </div>
        {status === 'failed' && transfer.reason ? (
          <div className="mono truncate text-[11px] text-[var(--color-status-red)]">
            {transfer.reason}
          </div>
        ) : null}
        {status === 'completed' && transfer.sha256 ? (
          <div className="mono truncate text-[10.5px] text-[var(--color-text-muted)]">
            sha256: {shortHash(transfer.sha256, 12)}
          </div>
        ) : null}
      </div>

      <div className="mono truncate text-[11px] text-[var(--color-text-secondary)]">
        {transfer.peerName ?? '—'}
      </div>

      <div className="min-w-0">
        {status === 'active' ? (
          <>
            <Progress
              value={pct}
              tone={transfer.direction === 'send' ? 'info' : 'success'}
            />
            <div className="mt-0.5 flex items-center justify-between text-[10.5px] text-[var(--color-text-secondary)]">
              <span className="mono">
                {formatBytes(bytesTransferred)}
                {totalBytes ? ` / ${formatBytes(totalBytes)}` : ''}
              </span>
              <span className="mono">{formatSpeed(speedBps)}</span>
            </div>
          </>
        ) : status === 'completed' ? (
          <div className="mono text-[11px] text-[var(--color-text-secondary)]">
            {formatBytes(bytesTransferred)}
            {transfer.elapsedSecs !== undefined
              ? ` · ${formatElapsed(transfer.elapsedSecs)}`
              : ''}
          </div>
        ) : status === 'pending' ? (
          <div className="mono text-[11px] text-[var(--color-text-muted)]">
            pending…
          </div>
        ) : (
          <div className="mono text-[11px] text-[var(--color-status-red)]">
            failed
          </div>
        )}
      </div>

      <div className="mono truncate text-[10.5px] text-[var(--color-text-muted)]">
        {transfer.direction === 'send' ? 'sent' : 'received'}
        {status === 'completed' && transfer.savedPath
          ? ` → ${transfer.savedPath}`
          : ''}
      </div>
    </div>
  );
}
