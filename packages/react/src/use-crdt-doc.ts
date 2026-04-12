import { useEffect, useRef, useState } from 'react';
import type { NapiNode } from '@vibecook/truffle';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/** Shape of the NAPI CrdtDoc instance returned by `node.crdtDoc()`. */
interface NapiCrdtDoc {
  getDeepValue(): Record<string, unknown>;
  commit(): void;
  docId(): string;
  mapInsert(container: string, key: string, value: unknown): void;
  mapDelete(container: string, key: string): void;
  listPush(container: string, value: unknown): void;
  listInsert(container: string, index: number, value: unknown): void;
  listDelete(container: string, index: number, len: number): void;
  textInsert(container: string, pos: number, text: string): void;
  textDelete(container: string, pos: number, len: number): void;
  counterIncrement(container: string, value: number): void;
  onChange(callback: (event: CrdtDocEvent) => void): void;
  stop(): Promise<void>;
}

/** Events emitted by the CRDT document. */
interface CrdtDocEvent {
  eventType: 'local_change' | 'remote_change' | 'peer_synced' | 'peer_left';
  peerId?: string;
}

export interface UseCrdtDocResult {
  /** The NapiCrdtDoc instance, or null before initialization completes. */
  doc: NapiCrdtDoc | null;
  /** Reactive snapshot of the full document state (from `doc.getDeepValue()`). */
  value: Record<string, unknown>;
  /** True after the doc is created and the initial value has been loaded. */
  isReady: boolean;
}

/**
 * React hook for a Truffle CRDT document.
 *
 * Provides reactive access to a Loro-backed CRDT document that is
 * automatically synchronized across the mesh. The returned `value`
 * updates on every local or remote change.
 *
 * @example
 * ```tsx
 * const { doc, value, isReady } = useCrdtDoc(node, 'my-doc');
 *
 * // Write to the document
 * if (doc) {
 *   doc.mapInsert('root', 'title', 'Hello');
 *   doc.commit();
 * }
 *
 * // Read the reactive state
 * console.log(value); // { root: { title: 'Hello' } }
 * ```
 */
export function useCrdtDoc(node: NapiNode | null, docId: string): UseCrdtDocResult {
  const [doc, setDoc] = useState<NapiCrdtDoc | null>(null);
  const [value, setValue] = useState<Record<string, unknown>>({});
  const [isReady, setIsReady] = useState(false);
  const docRef = useRef<NapiCrdtDoc | null>(null);

  useEffect(() => {
    if (!node) {
      setDoc(null);
      setValue({});
      setIsReady(false);
      return;
    }

    let cancelled = false;
    // Track the pending doc so cleanup can reach it even if the await
    // hasn't resolved yet (fixes leak on rapid node/docId changes).
    let pendingDoc: NapiCrdtDoc | null = null;

    const init = async () => {
      try {
        // `crdtDoc` is not in the generated .d.ts yet (it was added after
        // the last NAPI binding generation), so we cast through `any`.
        const d: NapiCrdtDoc = (node as any).crdtDoc(docId);
        pendingDoc = d;
        if (cancelled) {
          await d.stop();
          return;
        }
        docRef.current = d;
        setDoc(d);

        // Load initial value
        const initial = d.getDeepValue();
        if (!cancelled) {
          setValue(initial ?? {});
          setIsReady(true);
        }

        // Subscribe to local + remote changes
        d.onChange((_event: CrdtDocEvent) => {
          if (cancelled) return;
          const updated = d.getDeepValue();
          setValue(updated ?? {});
        });
      } catch (err) {
        console.error('useCrdtDoc: failed to initialize', err);
      }
    };

    init();

    return () => {
      cancelled = true;
      const docToStop = docRef.current ?? pendingDoc;
      if (docToStop) {
        docToStop.stop().catch(() => {});
      }
      docRef.current = null;
      pendingDoc = null;
      setDoc(null);
      setValue({});
      setIsReady(false);
    };
  }, [node, docId]);

  return { doc, value, isReady };
}
