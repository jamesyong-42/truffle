import { useCallback, useEffect, useState } from 'react';
import type { HealthInfo, PingResult } from '@shared/ipc';

interface UseHealthResult {
  health: HealthInfo | null;
  ping: (peerId: string) => Promise<PingResult>;
}

/**
 * Health polling hook. Pulls an initial snapshot and subscribes to
 * `onHealthUpdate` events from the main process.
 */
export function useHealth(): UseHealthResult {
  const [health, setHealth] = useState<HealthInfo | null>(null);

  useEffect(() => {
    let cancelled = false;

    void window.truffle
      .health()
      .then((info) => {
        if (!cancelled) setHealth(info);
      })
      .catch(() => {
        // Node may not be started yet; ignore.
      });

    const unsub = window.truffle.onHealthUpdate((info) => {
      setHealth(info);
    });

    return () => {
      cancelled = true;
      unsub();
    };
  }, []);

  const ping = useCallback((peerId: string) => {
    return window.truffle.ping(peerId);
  }, []);

  return { health, ping };
}
