import { useEffect, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';

interface AuthGateProps {
  authUrl: string;
  onOpen: (url: string) => void | Promise<void>;
}

/**
 * Full-screen overlay shown when Tailscale requests authentication.
 * Focus-trapped; the "Open in Browser" button is the first tab stop.
 */
export function AuthGate({ authUrl, onOpen }: AuthGateProps) {
  const [copied, setCopied] = useState(false);
  const openButtonRef = useRef<HTMLButtonElement | null>(null);
  const dialogRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    openButtonRef.current?.focus();
  }, [authUrl]);

  // Rudimentary focus trap — cycle Tab between focusable children of the
  // dialog only.
  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;

    const handleKey = (e: KeyboardEvent) => {
      if (e.key !== 'Tab') return;
      const focusables = dialog.querySelectorAll<HTMLElement>(
        'button, [href], input, textarea, select, [tabindex]:not([tabindex="-1"])',
      );
      if (focusables.length === 0) return;
      const first = focusables[0];
      const last = focusables[focusables.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };

    document.addEventListener('keydown', handleKey);
    return () => document.removeEventListener('keydown', handleKey);
  }, []);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(authUrl);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      setCopied(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-md"
      role="dialog"
      aria-modal="true"
      aria-labelledby="auth-title"
    >
      <div
        ref={dialogRef}
        className="mx-6 w-full max-w-xl rounded-xl border border-[var(--color-border-subtle)] bg-[var(--color-surface)] p-8 shadow-2xl"
      >
        <div className="mb-4 flex items-center gap-2 text-[var(--color-accent-hover)]">
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
          <span className="font-semibold tracking-tight">Truffle</span>
        </div>

        <h1
          id="auth-title"
          className="mb-2 text-[18px] font-semibold text-[var(--color-text-primary)]"
        >
          Tailscale authentication required
        </h1>
        <p className="mb-5 text-[13px] text-[var(--color-text-secondary)]">
          Open the URL below to authenticate this device with your tailnet.
          This window will update automatically once authentication completes.
        </p>

        <div className="mb-5 flex items-stretch gap-2">
          <div className="mono flex min-w-0 flex-1 items-center overflow-x-auto rounded-md border border-[var(--color-border-subtle)] bg-[var(--color-bg)] px-3 py-2 text-[12px] text-[var(--color-text-code)] selectable">
            {authUrl}
          </div>
          <Button
            variant="secondary"
            size="md"
            onClick={() => void copy()}
            aria-label="Copy authentication URL"
          >
            {copied ? 'Copied!' : 'Copy'}
          </Button>
        </div>

        <div className="flex items-center justify-between gap-3">
          <Button
            ref={openButtonRef}
            size="lg"
            onClick={() => void onOpen(authUrl)}
          >
            Open in Browser
          </Button>
          <span className="text-[11px] text-[var(--color-text-muted)]">
            Waiting for authentication…
          </span>
        </div>
      </div>
    </div>
  );
}
