import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import type {
  HealthInfo,
  NodeIdentity,
  NodeState,
  StartConfig,
} from '@shared/ipc';

interface TruffleContextValue {
  /** Node identity (id, name, ip) once the node has started. */
  nodeIdentity: NodeIdentity | null;
  /** Lifecycle phase the main process reports. */
  nodeState: NodeState;
  /** `true` once the node has emitted a `running` state. */
  isStarted: boolean;
  /** `true` while `start()` is in flight. */
  isStarting: boolean;
  /** The most recent Tailscale auth URL, or `null` when authenticated. */
  authUrl: string | null;
  /** Last start error, or `null` if none. */
  startError: string | null;
  /** Latest polled health info, or `null` until the first push. */
  health: HealthInfo | null;
  /** Imperatively (re)start the node with a specific config. */
  startNode: (config: StartConfig) => Promise<void>;
  /** Stop the node. */
  stopNode: () => Promise<void>;
  /** Clear the auth URL (the overlay closes). */
  clearAuthUrl: () => void;
}

const TruffleContext = createContext<TruffleContextValue | null>(null);

/**
 * Build a default start config. Random suffix keeps multiple dev launches
 * from colliding in the Tailscale admin UI.
 */
function makeDefaultConfig(): StartConfig {
  const rand = Math.random().toString(36).slice(2, 8);
  return {
    name: `playground-${rand}`,
    ephemeral: true,
  };
}

interface TruffleProviderProps {
  children: ReactNode;
}

export function TruffleProvider({ children }: TruffleProviderProps) {
  const [nodeIdentity, setNodeIdentity] = useState<NodeIdentity | null>(null);
  const [nodeState, setNodeState] = useState<NodeState>('idle');
  const [isStarting, setIsStarting] = useState(false);
  const [authUrl, setAuthUrl] = useState<string | null>(null);
  const [startError, setStartError] = useState<string | null>(null);
  const [health, setHealth] = useState<HealthInfo | null>(null);

  // Guard against double-start in StrictMode dev mount/unmount/mount.
  const autoStartedRef = useRef(false);

  const startNode = useCallback(async (config: StartConfig) => {
    setIsStarting(true);
    setStartError(null);
    try {
      const identity = await window.truffle.start(config);
      setNodeIdentity(identity);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setStartError(msg);
      throw err;
    } finally {
      setIsStarting(false);
    }
  }, []);

  const stopNode = useCallback(async () => {
    await window.truffle.stop();
    setNodeIdentity(null);
    setNodeState('idle');
  }, []);

  const clearAuthUrl = useCallback(() => setAuthUrl(null), []);

  // Subscribe to main process events, pull initial state, and auto-start.
  useEffect(() => {
    const api = window.truffle;
    if (!api) {
      setStartError(
        'window.truffle bridge is not available — preload failed to load.',
      );
      return;
    }

    const unsubAuth = api.onAuthRequired((url) => {
      setAuthUrl(url);
    });

    const unsubState = api.onNodeState((event) => {
      setNodeState(event.state);
      if (event.identity) setNodeIdentity(event.identity);
      if (event.state === 'running') setAuthUrl(null);
      if (event.state === 'error' && event.error) setStartError(event.error);
    });

    const unsubHealth = api.onHealthUpdate((info) => {
      setHealth(info);
    });

    // Pull initial snapshots. These may race with the auto-start below, so
    // tolerate partial state gracefully.
    void api.getNodeState().then((evt) => {
      setNodeState(evt.state);
      if (evt.identity) setNodeIdentity(evt.identity);
    });
    void api.health().then((info) => setHealth(info)).catch(() => {});

    // Auto-start the node once.
    if (!autoStartedRef.current) {
      autoStartedRef.current = true;
      void startNode(makeDefaultConfig()).catch(() => {
        // error already set by startNode
      });
    }

    return () => {
      unsubAuth();
      unsubState();
      unsubHealth();
    };
  }, [startNode]);

  const value = useMemo<TruffleContextValue>(
    () => ({
      nodeIdentity,
      nodeState,
      isStarted: nodeState === 'running',
      isStarting,
      authUrl,
      startError,
      health,
      startNode,
      stopNode,
      clearAuthUrl,
    }),
    [
      nodeIdentity,
      nodeState,
      isStarting,
      authUrl,
      startError,
      health,
      startNode,
      stopNode,
      clearAuthUrl,
    ],
  );

  return (
    <TruffleContext.Provider value={value}>{children}</TruffleContext.Provider>
  );
}

export function useTruffle(): TruffleContextValue {
  const ctx = useContext(TruffleContext);
  if (!ctx) {
    throw new Error('useTruffle must be used within <TruffleProvider>');
  }
  return ctx;
}
