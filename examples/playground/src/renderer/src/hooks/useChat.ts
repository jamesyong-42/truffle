import { useCallback, useEffect, useState } from 'react';
import type { ChatMessage } from '@shared/ipc';

const MAX_MESSAGES = 500;

interface UseChatResult {
  messages: ChatMessage[];
  send: (peerId: string, text: string) => Promise<void>;
  broadcast: (text: string) => Promise<void>;
  clear: () => void;
}

/**
 * In-memory chat message log. Capped at MAX_MESSAGES entries.
 *
 * The main process emits a single `onMessage` stream that includes both
 * direct and broadcast messages; broadcast messages carry `broadcast: true`.
 * Outgoing messages the local user sends are NOT echoed by truffle, so we
 * append them here locally when `send`/`broadcast` resolves.
 */
export function useChat(): UseChatResult {
  const [messages, setMessages] = useState<ChatMessage[]>([]);

  useEffect(() => {
    const unsub = window.truffle.onMessage((msg) => {
      setMessages((current) => {
        const next = [...current, msg];
        if (next.length > MAX_MESSAGES) {
          return next.slice(next.length - MAX_MESSAGES);
        }
        return next;
      });
    });
    return () => unsub();
  }, []);

  const appendLocal = useCallback((msg: ChatMessage) => {
    setMessages((current) => {
      const next = [...current, msg];
      if (next.length > MAX_MESSAGES) {
        return next.slice(next.length - MAX_MESSAGES);
      }
      return next;
    });
  }, []);

  const send = useCallback(
    async (peerId: string, text: string) => {
      await window.truffle.sendMessage(peerId, text);
      appendLocal({
        from: '__self__',
        fromName: 'you',
        text,
        ts: Date.now(),
      });
    },
    [appendLocal],
  );

  const broadcast = useCallback(
    async (text: string) => {
      await window.truffle.broadcast(text);
      appendLocal({
        from: '__self__',
        fromName: 'you',
        text,
        ts: Date.now(),
        broadcast: true,
      });
    },
    [appendLocal],
  );

  const clear = useCallback(() => setMessages([]), []);

  return { messages, send, broadcast, clear };
}
