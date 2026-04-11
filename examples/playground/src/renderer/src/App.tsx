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

/**
 * True on macOS. The window uses `titleBarStyle: hiddenInset` on mac,
 * which overlays the native traffic lights in the top-left — the drag
 * bar needs extra left padding so the nav/logo doesn't collide with them.
 */
const isMac =
  typeof navigator !== 'undefined' && /Mac/i.test(navigator.platform);

/**
 * Draggable strip at the top of the window. Required because
 * `titleBarStyle: hiddenInset` removes the native title bar and leaves
 * no way to move the window without a CSS drag region. On macOS it
 * leaves 72px of clearance for the native traffic light buttons.
 */
function TitleBar() {
  return (
    <div
      className="drag-region flex h-9 shrink-0 items-center border-b border-[var(--color-border-subtle)] bg-[var(--color-bg)]"
      style={{ paddingLeft: isMac ? 80 : 12, paddingRight: 12 }}
    >
      <div className="flex items-center gap-2 text-[11px] font-semibold uppercase tracking-[0.12em] text-[var(--color-text-secondary)]">
        <svg
          width="12"
          height="12"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.2"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
          className="text-[var(--color-accent-hover)]"
        >
          <circle cx="12" cy="12" r="9" />
          <path d="M12 3v18" />
          <path d="M3 12h18" />
        </svg>
        Truffle Playground
      </div>
    </div>
  );
}

/**
 * Spinner shown while the sidecar is coming up but before any peer
 * events, auth URLs, or errors have been emitted.
 */
function StartingSpinner({ phase }: { phase: string }) {
  return (
    <div className="flex h-full w-full items-center justify-center">
      <div className="flex flex-col items-center gap-4 text-center">
        <div
          aria-hidden="true"
          className="h-8 w-8 animate-spin rounded-full border-2 border-[var(--color-border-subtle)] border-t-[var(--color-accent)]"
        />
        <div className="text-[13px] text-[var(--color-text-primary)]">
          Starting Truffle node
        </div>
        <div className="text-[11px] text-[var(--color-text-secondary)]">
          {phase}
        </div>
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
        <p className="mono mb-4 text-[11.5px] text-[var(--color-text-secondary)] selectable whitespace-pre-wrap">
          {error}
        </p>
        <Button onClick={onRetry} variant="secondary">
          Retry
        </Button>
      </div>
    </div>
  );
}

/**
 * The main UI shell rendered once the node is running. Hooks are
 * intentionally scoped to this component so they don't fire IPC calls
 * (like `getPeers()` and `health()`) before the main process has a
 * live `NapiNode` — those calls would otherwise throw "Node not started"
 * on every mount.
 */
function RunningShell() {
  const [activePanel, setActivePanel] = useState<PanelId>('peers');
  const { nodeIdentity, health } = useTruffle();
  const { peers } = usePeers();
  const { pendingOffers, transfers } = useFileTransfer();

  const activeTransferCount = transfers.filter(
    (t) => t.status === 'active',
  ).length;

  let content: React.ReactNode;
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

  return (
    <div className="flex h-screen w-screen flex-col bg-[var(--color-bg)] text-[var(--color-text-primary)]">
      <TitleBar />
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
    </div>
  );
}

/**
 * Chromeless shell used during the pre-running lifecycle phases
 * (starting, auth required, error). It provides the same window
 * background + layout as RunningShell so the transition is seamless.
 */
function StartupShell({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex h-screen w-screen flex-col bg-[var(--color-bg)] text-[var(--color-text-primary)]">
      <TitleBar />
      <main className="min-w-0 flex-1 overflow-hidden">{children}</main>
    </div>
  );
}

function AppInner() {
  const { isStarted, isStarting, authUrl, startError, startNode } =
    useTruffle();

  const openAuth = (url: string) => window.truffle.openAuthUrl(url);

  const retry = () => {
    // Retry with a randomised `deviceName` so two playgrounds on the
    // same machine don't trip over the same Tailscale hostname slug.
    // `ephemeral: true` keeps the tailnet entry from sticking around.
    void startNode({
      appId: 'playground',
      deviceName: `playground-${Math.random().toString(36).slice(2, 8)}`,
      ephemeral: true,
    });
  };

  // Error supersedes every other state.
  if (startError && !isStarting) {
    return (
      <StartupShell>
        <ErrorState error={startError} onRetry={retry} />
      </StartupShell>
    );
  }

  // Auth URL is the most actionable thing — promote it to the primary
  // screen instead of overlaying a spinner behind a modal.
  if (!isStarted && authUrl) {
    return (
      <StartupShell>
        <AuthGate authUrl={authUrl} onOpen={openAuth} />
      </StartupShell>
    );
  }

  // Still booting the sidecar and no auth URL yet — pure progress state.
  if (!isStarted) {
    const phase = isStarting
      ? 'Launching Tailscale sidecar and connecting to control plane…'
      : 'Initializing…';
    return (
      <StartupShell>
        <StartingSpinner phase={phase} />
      </StartupShell>
    );
  }

  // Node is running — mount the real shell with hooks.
  return <RunningShell />;
}

export default function App() {
  return (
    <TruffleProvider>
      <AppInner />
    </TruffleProvider>
  );
}
