import { useState } from 'react';
import { TruffleProvider, useTruffle } from '@/context/TruffleContext';
import { usePeers } from '@/hooks/usePeers';
import { useFileTransfer } from '@/hooks/useFileTransfer';
import { Sidebar, type PanelId } from '@/components/layout/Sidebar';
import { StatusBar } from '@/components/layout/StatusBar';
import { AuthGate } from '@/components/auth/AuthGate';
import { PeersPanel } from '@/components/panels/PeersPanel';
import { ChatPanel } from '@/components/panels/ChatPanel';
import { StorePanel } from '@/components/panels/StorePanel';
import { FilesPanel } from '@/components/panels/FilesPanel';
import { HealthPanel } from '@/components/panels/HealthPanel';
import { Button } from '@/components/ui/button';

function StartingSpinner() {
  return (
    <div className="flex h-full w-full items-center justify-center">
      <div className="flex flex-col items-center gap-3 text-[12px] text-[var(--color-text-secondary)]">
        <div
          aria-hidden="true"
          className="h-6 w-6 animate-spin rounded-full border-2 border-[var(--color-border-subtle)] border-t-[var(--color-accent)]"
        />
        Starting Truffle node…
      </div>
    </div>
  );
}

interface ErrorStateProps {
  error: string;
  onRetry: () => void;
}

function ErrorState({ error, onRetry }: ErrorStateProps) {
  return (
    <div className="flex h-full w-full items-center justify-center p-10">
      <div className="max-w-md rounded-lg border border-[var(--color-status-red)]/40 bg-[var(--color-status-red)]/10 p-6 text-center">
        <h3 className="mb-2 text-[14px] font-semibold text-[var(--color-status-red)]">
          Failed to start Truffle node
        </h3>
        <p className="mono mb-4 text-[11.5px] text-[var(--color-text-secondary)] selectable">
          {error}
        </p>
        <Button onClick={onRetry} variant="secondary">
          Retry
        </Button>
      </div>
    </div>
  );
}

function AppInner() {
  const [activePanel, setActivePanel] = useState<PanelId>('peers');
  const {
    nodeIdentity,
    isStarted,
    isStarting,
    authUrl,
    startError,
    health,
    startNode,
  } = useTruffle();

  // These hooks are shared across the shell (sidebar badges, status bar)
  // AND the panels. Both call sites subscribe to the same events; duplication
  // is cheap because each hook maintains its own copy and the event handlers
  // are pure. Alternatively we could hoist them into the context; keeping
  // them co-located with their consumers is simpler.
  const { peers } = usePeers();
  const { pendingOffers, transfers } = useFileTransfer();
  const activeTransferCount = transfers.filter(
    (t) => t.status === 'active',
  ).length;

  const openAuth = (url: string) => window.truffle.openAuthUrl(url);

  const retry = () => {
    void startNode({
      name: `playground-${Math.random().toString(36).slice(2, 8)}`,
      ephemeral: true,
    });
  };

  let content: React.ReactNode;
  if (startError && !isStarting) {
    content = <ErrorState error={startError} onRetry={retry} />;
  } else if (!isStarted && (isStarting || !nodeIdentity)) {
    content = <StartingSpinner />;
  } else {
    switch (activePanel) {
      case 'peers':
        content = <PeersPanel />;
        break;
      case 'chat':
        content = <ChatPanel />;
        break;
      case 'store':
        content = <StorePanel />;
        break;
      case 'files':
        content = <FilesPanel />;
        break;
      case 'health':
        content = <HealthPanel />;
        break;
      default:
        content = null;
    }
  }

  return (
    <div className="flex h-screen w-screen flex-col bg-[var(--color-bg)] text-[var(--color-text-primary)]">
      <div className="flex min-h-0 flex-1">
        <Sidebar
          activePanel={activePanel}
          onSelect={setActivePanel}
          peerCount={peers.length}
          pendingOfferCount={pendingOffers.length}
          activeTransferCount={activeTransferCount}
        />
        <main className="min-w-0 flex-1 overflow-hidden">{content}</main>
      </div>
      <StatusBar
        identity={nodeIdentity}
        health={health}
        peerCount={peers.length}
      />
      {authUrl ? <AuthGate authUrl={authUrl} onOpen={openAuth} /> : null}
    </div>
  );
}

export default function App() {
  return (
    <TruffleProvider>
      <AppInner />
    </TruffleProvider>
  );
}
