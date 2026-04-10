import type { ReactNode } from 'react';
import { cn } from '@/lib/cn';
import { Tooltip } from '@/components/ui/tooltip';

export type PanelId = 'peers' | 'chat' | 'store' | 'files' | 'health';

interface NavItemDef {
  id: PanelId;
  label: string;
  icon: ReactNode;
  badge?: number;
}

interface SidebarProps {
  activePanel: PanelId;
  onSelect: (panel: PanelId) => void;
  peerCount: number;
  pendingOfferCount: number;
  activeTransferCount: number;
}

// Inline SVG icons — hand-drawn with the lucide stroke style so we don't
// have to install lucide-react.
const IconPeers = (
  <svg
    width="18"
    height="18"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.8"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2" />
    <circle cx="9" cy="7" r="4" />
    <path d="M23 21v-2a4 4 0 0 0-3-3.87" />
    <path d="M16 3.13a4 4 0 0 1 0 7.75" />
  </svg>
);

const IconChat = (
  <svg
    width="18"
    height="18"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.8"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
  </svg>
);

const IconStore = (
  <svg
    width="18"
    height="18"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.8"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <ellipse cx="12" cy="5" rx="9" ry="3" />
    <path d="M3 5v14a9 3 0 0 0 18 0V5" />
    <path d="M3 12a9 3 0 0 0 18 0" />
  </svg>
);

const IconFiles = (
  <svg
    width="18"
    height="18"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.8"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
    <polyline points="14 2 14 8 20 8" />
    <path d="M12 18v-6" />
    <polyline points="9 15 12 12 15 15" />
  </svg>
);

const IconHealth = (
  <svg
    width="18"
    height="18"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.8"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M22 12h-4l-3 9L9 3l-3 9H2" />
  </svg>
);

const IconWordmark = (
  <svg
    width="22"
    height="22"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.8"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <circle cx="12" cy="12" r="9" />
    <path d="M12 3v18" />
    <path d="M3 12h18" />
  </svg>
);

export function Sidebar({
  activePanel,
  onSelect,
  peerCount,
  pendingOfferCount,
  activeTransferCount,
}: SidebarProps) {
  const items: NavItemDef[] = [
    { id: 'peers', label: 'Peers', icon: IconPeers, badge: peerCount },
    { id: 'chat', label: 'Chat', icon: IconChat },
    { id: 'store', label: 'Synced Store', icon: IconStore },
    {
      id: 'files',
      label: 'Files',
      icon: IconFiles,
      badge: pendingOfferCount + activeTransferCount,
    },
    { id: 'health', label: 'Health', icon: IconHealth },
  ];
  return (
    <nav
      role="navigation"
      aria-label="Main navigation"
      className="flex h-full w-14 shrink-0 flex-col items-center border-r border-[var(--color-border-subtle)] bg-[var(--color-surface)] py-3"
    >
      <div className="mb-3 flex h-10 w-10 items-center justify-center text-[var(--color-accent-hover)]">
        {IconWordmark}
      </div>
      <div className="flex flex-1 flex-col items-center gap-1">
        {items.map((item) => {
          const isActive = activePanel === item.id;
          return (
            <Tooltip key={item.id} label={item.label} side="right">
              <button
                type="button"
                aria-label={item.label}
                aria-current={isActive ? 'page' : undefined}
                onClick={() => onSelect(item.id)}
                className={cn(
                  'relative flex h-10 w-10 items-center justify-center rounded-md transition-colors',
                  'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)] focus-visible:ring-offset-2 focus-visible:ring-offset-[var(--color-surface)]',
                  isActive
                    ? 'bg-[var(--color-surface-raised)] text-[var(--color-text-primary)]'
                    : 'text-[var(--color-text-secondary)] hover:bg-[var(--color-surface-raised)] hover:text-[var(--color-text-primary)]',
                )}
              >
                {isActive ? (
                  <span
                    aria-hidden="true"
                    className="absolute left-0 top-1/2 h-5 w-0.5 -translate-y-1/2 rounded-r bg-[var(--color-accent)]"
                  />
                ) : null}
                {item.icon}
                {item.badge && item.badge > 0 ? (
                  <span
                    aria-hidden="true"
                    className="mono absolute -right-0.5 -top-0.5 flex h-4 min-w-[16px] items-center justify-center rounded-full bg-[var(--color-accent)] px-1 text-[9.5px] font-bold text-white"
                  >
                    {item.badge > 99 ? '99+' : item.badge}
                  </span>
                ) : null}
              </button>
            </Tooltip>
          );
        })}
      </div>
    </nav>
  );
}
