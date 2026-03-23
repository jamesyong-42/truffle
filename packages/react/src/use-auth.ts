import { useState, useEffect, useCallback } from 'react';
import type { NapiMeshNode as MeshNode } from '@vibecook/truffle';

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
 * Polls the MeshNode's auth status to stay in sync.
 * Works alongside useMesh — does not consume onEvent.
 */
export function useAuth(node: MeshNode | null): UseAuthResult {
  const [status, setStatus] = useState<AuthStatus>('unknown');
  const [authUrl, setAuthUrl] = useState<string | null>(null);

  useEffect(() => {
    if (!node) return;
    let cancelled = false;

    const fetchAuth = async () => {
      try {
        const [s, url] = await Promise.all([
          node.authStatus(),
          node.authUrl(),
        ]);
        if (cancelled) return;
        setStatus(s as AuthStatus);
        setAuthUrl(url);
      } catch {
        // Node may not be started yet
      }
    };

    fetchAuth();

    // Auth state changes rarely (2-3 times per session).
    // 1-second polling is acceptable and avoids consuming onEvent.
    const interval = setInterval(fetchAuth, 1000);

    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [node]);

  return {
    status,
    authUrl,
    isAuthRequired: status === 'required',
    isAuthenticated: status === 'authenticated',
  };
}
