import { useCallback, useEffect, useState } from 'react';
import type {
  FileOffer,
  TransferCompleted,
  TransferFailed,
  TransferProgress,
  TransferResult,
} from '@shared/ipc';

/**
 * UI-facing transfer record. Not defined in `@shared/ipc` because the main
 * process emits separate progress/completed/failed events — the renderer
 * stitches them into a single merged record keyed by token.
 */
export interface TransferState {
  token: string;
  direction: 'send' | 'receive';
  fileName: string;
  /** May be `null` while the offer/send is in the earliest phase. */
  totalBytes: number | null;
  bytesTransferred: number;
  speedBps: number;
  status: 'pending' | 'active' | 'completed' | 'failed';
  /** Populated on completion. */
  sha256?: string;
  elapsedSecs?: number;
  savedPath?: string;
  /** Populated on failure. */
  reason?: string;
  /** Sort key — when the transfer last changed. */
  updatedAt: number;
  /** Set when we know which peer is on the other end. */
  peerId?: string;
  peerName?: string;
}

interface UseFileTransferResult {
  transfers: TransferState[];
  pendingOffers: FileOffer[];
  sendFile: (peerId: string, localPath: string) => Promise<TransferResult>;
  accept: (token: string, savePath: string) => Promise<void>;
  reject: (token: string, reason?: string) => Promise<void>;
}

export function useFileTransfer(): UseFileTransferResult {
  const [transferMap, setTransferMap] = useState<Map<string, TransferState>>(
    () => new Map(),
  );
  const [pendingOffers, setPendingOffers] = useState<FileOffer[]>([]);

  useEffect(() => {
    const upsert = (
      token: string,
      updater: (prev: TransferState | undefined) => TransferState,
    ) => {
      setTransferMap((current) => {
        const next = new Map(current);
        next.set(token, updater(current.get(token)));
        return next;
      });
    };

    const unsubOffer = window.truffle.onFileOffer((offer: FileOffer) => {
      setPendingOffers((current) => {
        if (current.some((o) => o.token === offer.token)) return current;
        return [...current, offer];
      });
      // Seed a pending transfer row so the offer shows up in history as well.
      upsert(offer.token, () => ({
        token: offer.token,
        direction: 'receive',
        fileName: offer.fileName,
        totalBytes: offer.size,
        bytesTransferred: 0,
        speedBps: 0,
        status: 'pending',
        updatedAt: Date.now(),
        peerId: offer.fromPeer,
        peerName: offer.fromName,
      }));
    });

    const unsubProgress = window.truffle.onFileProgress(
      (progress: TransferProgress) => {
        upsert(progress.token, (prev) => ({
          token: progress.token,
          direction: progress.direction,
          fileName: progress.fileName,
          totalBytes: progress.totalBytes,
          bytesTransferred: progress.bytesTransferred,
          speedBps: progress.speedBps,
          status: 'active',
          updatedAt: Date.now(),
          peerId: prev?.peerId,
          peerName: prev?.peerName,
          sha256: prev?.sha256,
          elapsedSecs: prev?.elapsedSecs,
          savedPath: prev?.savedPath,
          reason: prev?.reason,
        }));
      },
    );

    const unsubCompleted = window.truffle.onFileCompleted(
      (result: TransferCompleted) => {
        upsert(result.token, (prev) => ({
          token: result.token,
          direction: result.direction,
          fileName: result.fileName,
          totalBytes: result.bytesTransferred,
          bytesTransferred: result.bytesTransferred,
          speedBps: 0,
          status: 'completed',
          sha256: result.sha256,
          elapsedSecs: result.elapsedSecs,
          savedPath: result.savedPath,
          updatedAt: Date.now(),
          peerId: prev?.peerId,
          peerName: prev?.peerName,
        }));
        // Clear any matching pending offer.
        setPendingOffers((current) =>
          current.filter((o) => o.token !== result.token),
        );
      },
    );

    const unsubFailed = window.truffle.onFileFailed((err: TransferFailed) => {
      upsert(err.token, (prev) => ({
        token: err.token,
        direction: err.direction ?? prev?.direction ?? 'receive',
        fileName: err.fileName ?? prev?.fileName ?? 'unknown',
        totalBytes: prev?.totalBytes ?? null,
        bytesTransferred: prev?.bytesTransferred ?? 0,
        speedBps: 0,
        status: 'failed',
        reason: err.reason,
        updatedAt: Date.now(),
        peerId: prev?.peerId,
        peerName: prev?.peerName,
      }));
      setPendingOffers((current) =>
        current.filter((o) => o.token !== err.token),
      );
    });

    return () => {
      unsubOffer();
      unsubProgress();
      unsubCompleted();
      unsubFailed();
    };
  }, []);

  const sendFile = useCallback(
    async (peerId: string, localPath: string): Promise<TransferResult> => {
      return window.truffle.sendFile(peerId, localPath);
    },
    [],
  );

  const accept = useCallback(async (token: string, savePath: string) => {
    await window.truffle.acceptOffer(token, savePath);
    setPendingOffers((current) => current.filter((o) => o.token !== token));
  }, []);

  const reject = useCallback(async (token: string, reason = 'declined') => {
    await window.truffle.rejectOffer(token, reason);
    setPendingOffers((current) => current.filter((o) => o.token !== token));
  }, []);

  // Sort by most-recently updated, newest first.
  const transfers = Array.from(transferMap.values()).sort(
    (a, b) => b.updatedAt - a.updatedAt,
  );

  return { transfers, pendingOffers, sendFile, accept, reject };
}
