import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
} from 'react';
import type { ChatMessage, Peer } from '@shared/ipc';
import { useChat } from '@/hooks/useChat';
import { usePeers } from '@/hooks/usePeers';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { ScrollArea } from '@/components/ui/scroll-area';
import { LogLine } from '@/components/shared/LogLine';
import { cn } from '@/lib/cn';

type Target = { kind: 'broadcast' } | { kind: 'peer'; peer: Peer };

export function ChatPanel() {
  const { messages, send, broadcast } = useChat();
  const { peers } = usePeers();
  const [input, setInput] = useState('');
  const [target, setTarget] = useState<Target>({ kind: 'broadcast' });
  const [targetPickerOpen, setTargetPickerOpen] = useState(false);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const stickToBottomRef = useRef(true);

  const sendablePeers = useMemo(
    () => peers.filter((p) => p.wsConnected),
    [peers],
  );

  // If the selected peer disappears, fall back to broadcast.
  useEffect(() => {
    if (target.kind === 'peer' && !sendablePeers.find((p) => p.deviceId === target.peer.deviceId)) {
      setTarget({ kind: 'broadcast' });
    }
  }, [sendablePeers, target]);

  // Auto-scroll to bottom when a new message arrives, unless the user
  // has scrolled up.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    if (stickToBottomRef.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [messages]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    stickToBottomRef.current = distance < 40;
  };

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    const text = input.trim();
    if (!text) return;
    if (target.kind === 'broadcast') {
      await broadcast(text);
    } else {
      await send(target.peer.deviceId, text);
    }
    setInput('');
    // Always stick to bottom after a send.
    stickToBottomRef.current = true;
  };

  const renderMessage = (msg: ChatMessage, idx: number) => {
    const isSelf = msg.from === '__self__';
    const variant = isSelf ? 'sent' : 'received';
    return (
      <LogLine
        key={`${msg.ts}-${idx}`}
        ts={msg.ts}
        text={msg.text}
        who={isSelf ? undefined : msg.fromName}
        broadcast={msg.broadcast}
        variant={variant}
      />
    );
  };

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-center justify-between border-b border-[var(--color-border-subtle)] px-4 py-2">
        <h2 className="section-header">Chat Messages</h2>
        <span className="text-[11px] text-[var(--color-text-muted)]">
          {messages.length} messages
        </span>
      </header>

      <ScrollArea
        ref={scrollRef}
        className="flex-1 px-4 py-2"
        onScroll={onScroll}
      >
        {messages.length === 0 ? (
          <div className="flex h-full items-center justify-center p-10 text-center text-[12px] text-[var(--color-text-muted)]">
            No messages yet. Type below to broadcast or send to a connected
            peer.
          </div>
        ) : (
          <div className="flex flex-col gap-1">
            {messages.map(renderMessage)}
          </div>
        )}
      </ScrollArea>

      <form
        onSubmit={(e) => void handleSubmit(e)}
        className="flex items-center gap-2 border-t border-[var(--color-border-subtle)] bg-[var(--color-surface)] px-3 py-2"
      >
        <div className="relative">
          <button
            type="button"
            onClick={() => setTargetPickerOpen((v) => !v)}
            aria-haspopup="listbox"
            aria-expanded={targetPickerOpen}
            className={cn(
              'mono flex h-8 items-center gap-1 rounded-md border border-[var(--color-border-subtle)] bg-[var(--color-surface-raised)] px-2.5 text-[12px] text-[var(--color-text-primary)]',
              'hover:border-[var(--color-accent)]/50',
              'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)] focus-visible:ring-offset-2 focus-visible:ring-offset-[var(--color-bg)]',
            )}
          >
            {target.kind === 'broadcast'
              ? 'To: broadcast'
              : `To: ${target.peer.deviceName}`}
            <svg
              width="10"
              height="10"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2.5"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
            >
              <polyline points="6 9 12 15 18 9" />
            </svg>
          </button>
          {targetPickerOpen ? (
            <div
              role="listbox"
              className="absolute bottom-full left-0 z-30 mb-1 min-w-[200px] overflow-hidden rounded-md border border-[var(--color-border-subtle)] bg-[var(--color-surface)] py-1 shadow-2xl"
            >
              <button
                type="button"
                role="option"
                aria-selected={target.kind === 'broadcast'}
                onClick={() => {
                  setTarget({ kind: 'broadcast' });
                  setTargetPickerOpen(false);
                }}
                className="flex w-full items-center px-3 py-1.5 text-left text-[12px] text-[var(--color-text-primary)] hover:bg-[var(--color-surface-raised)]"
              >
                Broadcast (all peers)
              </button>
              {sendablePeers.length === 0 ? (
                <div className="px-3 py-1.5 text-[11px] text-[var(--color-text-muted)]">
                  No connected peers
                </div>
              ) : (
                sendablePeers.map((peer) => (
                  <button
                    key={peer.deviceId}
                    type="button"
                    role="option"
                    aria-selected={
                      target.kind === 'peer' && target.peer.deviceId === peer.deviceId
                    }
                    onClick={() => {
                      setTarget({ kind: 'peer', peer });
                      setTargetPickerOpen(false);
                    }}
                    className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-[12px] text-[var(--color-text-primary)] hover:bg-[var(--color-surface-raised)]"
                  >
                    <span className="h-1.5 w-1.5 rounded-full bg-[var(--color-status-green)]" />
                    <span className="truncate">{peer.deviceName}</span>
                  </button>
                ))
              )}
            </div>
          ) : null}
        </div>

        <Input
          value={input}
          onChange={(e) => setInput(e.target.value)}
          placeholder={
            target.kind === 'broadcast'
              ? 'Broadcast to all peers…'
              : `Message ${target.peer.deviceName}…`
          }
          aria-label="Message text"
        />
        <Button type="submit" disabled={!input.trim()}>
          Send
        </Button>
      </form>
    </div>
  );
}
