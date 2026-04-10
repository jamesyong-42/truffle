import type { Peer } from '@shared/ipc';
import { cn } from '@/lib/cn';

interface PeerBadgeProps {
  peer: Peer;
  showIp?: boolean;
  className?: string;
}

function peerStatus(peer: Peer): {
  dotClass: string;
  label: string;
} {
  if (peer.wsConnected) {
    return {
      dotClass: 'bg-[var(--color-status-green)] shadow-[0_0_6px_rgba(34,197,94,0.65)]',
      label: 'Connected via WebSocket',
    };
  }
  if (peer.online) {
    return {
      dotClass: 'bg-[var(--color-status-amber)]',
      label: 'Online, not WebSocket-connected',
    };
  }
  return {
    dotClass: 'bg-[var(--color-status-red)]',
    label: 'Offline',
  };
}

export function PeerBadge({ peer, showIp = false, className }: PeerBadgeProps) {
  const { dotClass, label } = peerStatus(peer);
  return (
    <span
      className={cn('inline-flex items-center gap-2', className)}
      aria-label={`${peer.name}: ${label}`}
    >
      <span
        aria-hidden="true"
        className={cn('h-2 w-2 shrink-0 rounded-full', dotClass)}
      />
      <span className="truncate text-[var(--color-text-primary)]">
        {peer.name}
      </span>
      {showIp && peer.ip ? (
        <span className="mono text-[11px] text-[var(--color-text-secondary)]">
          {peer.ip}
        </span>
      ) : null}
    </span>
  );
}
