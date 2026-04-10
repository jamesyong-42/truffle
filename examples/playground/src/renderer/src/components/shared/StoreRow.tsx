import { useEffect, useState } from 'react';
import { cn } from '@/lib/cn';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';

interface StoreRowProps {
  k: string;
  v: string;
  editable?: boolean;
  onSave?: (key: string, value: string) => void | Promise<void>;
  onDelete?: (key: string) => void | Promise<void>;
}

export function StoreRow({
  k,
  v,
  editable = false,
  onSave,
  onDelete,
}: StoreRowProps) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(v);

  useEffect(() => {
    if (!editing) setDraft(v);
  }, [v, editing]);

  const commit = async () => {
    if (onSave && draft !== v) await onSave(k, draft);
    setEditing(false);
  };

  return (
    <div
      className={cn(
        'grid grid-cols-[140px_minmax(0,1fr)_auto] items-center gap-3',
        'border-b border-[var(--color-border-subtle)] px-3 py-1.5',
        'last:border-b-0 hover:bg-[var(--color-surface-raised)]/40',
      )}
    >
      <div className="mono text-[12px] text-[var(--color-text-code)] truncate selectable">
        {k}
      </div>
      {editing ? (
        <Input
          value={draft}
          autoFocus
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') void commit();
            if (e.key === 'Escape') {
              setDraft(v);
              setEditing(false);
            }
          }}
          className="h-7"
        />
      ) : (
        <div className="mono text-[12px] text-[var(--color-text-primary)] truncate selectable">
          {v}
        </div>
      )}
      {editable ? (
        <div className="flex items-center gap-1">
          {editing ? (
            <>
              <Button size="sm" variant="default" onClick={() => void commit()}>
                Save
              </Button>
              <Button
                size="sm"
                variant="ghost"
                onClick={() => {
                  setDraft(v);
                  setEditing(false);
                }}
              >
                Cancel
              </Button>
            </>
          ) : (
            <>
              <Button
                size="icon"
                variant="ghost"
                aria-label={`Edit ${k}`}
                onClick={() => setEditing(true)}
              >
                <svg
                  width="14"
                  height="14"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  aria-hidden="true"
                >
                  <path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" />
                  <path d="M18.5 2.5a2.121 2.121 0 1 1 3 3L12 15l-4 1 1-4 9.5-9.5z" />
                </svg>
              </Button>
              {onDelete ? (
                <Button
                  size="icon"
                  variant="ghost"
                  aria-label={`Delete ${k}`}
                  onClick={() => void onDelete(k)}
                >
                  <svg
                    width="14"
                    height="14"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    aria-hidden="true"
                  >
                    <path d="M3 6h18" />
                    <path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
                    <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
                  </svg>
                </Button>
              ) : null}
            </>
          )}
        </div>
      ) : (
        <div />
      )}
    </div>
  );
}
