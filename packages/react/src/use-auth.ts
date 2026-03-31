import { useState, useEffect } from 'react';
import type { NapiNode, NapiPeerEvent } from '@vibecook/truffle';

export type AuthStatus = 'unknown' | 'required' | 'authenticated';

export interface UseAuthResult {
  /** Current auth status. */
  status: AuthStatus;
  /** The Tailscale auth URL, if auth is required. */
  authUrl: string | null;
  /** Whether auth is currently required. */
  isAuthRequired: boolean;
  /** Whether the node is fully authenticated. */
  isAuthenticated: boolean;
}

/**
 * React hook for tracking Tailscale auth state.
 *
 * Listens for `auth_required` peer events from the Node API.
 * Once peers start arriving, auth is considered complete.
 *
 * @example
 * ```tsx
 * const { isAuthRequired, authUrl } = useAuth(node);
 *
 * if (isAuthRequired && authUrl) {
 *   return <a href={authUrl}>Authenticate with Tailscale</a>;
 * }
 * ```
 */
export function useAuth(node: NapiNode | null): UseAuthResult {
  const [status, setStatus] = useState<AuthStatus>('unknown');
  const [authUrl, setAuthUrl] = useState<string | null>(null);

  useEffect(() => {
    if (!node) return;
    let cancelled = false;

    // Check if we're already authenticated by trying to get peers
    const init = async () => {
      try {
        node.getLocalInfo(); // throws if not started
        if (!cancelled) setStatus('authenticated');
      } catch {
        // Not started yet — wait for events
      }
    };
    init();

    // Listen for auth events via onPeerChange
    node.onPeerChange((event: NapiPeerEvent) => {
      if (cancelled) return;

      if (event.eventType === 'auth_required' && event.authUrl) {
        setStatus('required');
        setAuthUrl(event.authUrl);
      } else if (event.eventType === 'joined') {
        // Peers arriving means auth succeeded
        setStatus('authenticated');
        setAuthUrl(null);
      }
    });

    return () => {
      cancelled = true;
    };
  }, [node]);

  return {
    status,
    authUrl,
    isAuthRequired: status === 'required',
    isAuthenticated: status === 'authenticated',
  };
}
