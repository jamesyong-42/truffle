import { execSync } from 'node:child_process';
import { NapiNode, type NapiNodeConfig, type NapiPeerEvent } from '@vibecook/truffle-native';
import { resolveSidecarPath } from './sidecar.js';

export interface CreateMeshNodeOptions {
  /** Node name (becomes the Tailscale hostname prefix). */
  name: string;
  /** Override sidecar path. If omitted, auto-resolved. */
  sidecarPath?: string;
  /** State directory for Tailscale. */
  stateDir?: string;
  /** Tailscale auth key (for headless/CI setups). */
  authKey?: string;
  /** Whether this node is ephemeral (removed from tailnet on stop). */
  ephemeral?: boolean;
  /** WebSocket listener port (0 = auto). */
  wsPort?: number;
  /**
   * Auto-open the Tailscale auth URL in the default browser.
   * Defaults to true. Set to false to handle auth manually.
   */
  autoAuth?: boolean;
  /**
   * Custom URL opener. Overrides the default browser opener.
   * Useful in Electron where you'd use `shell.openExternal()`.
   */
  openUrl?: (url: string) => void;
  /** Called when Tailscale auth is required. Receives the auth URL. */
  onAuthRequired?: (url: string) => void;
  /** Called for peer change events (join, leave, update, ws_connected, etc.). */
  onPeerChange?: (event: NapiPeerEvent) => void;
}

/**
 * Open a URL in the system's default browser.
 */
function defaultOpenUrl(url: string): void {
  try {
    if (process.platform === 'darwin') {
      execSync(`open "${url}"`);
    } else if (process.platform === 'win32') {
      execSync(`start "" "${url}"`);
    } else {
      execSync(`xdg-open "${url}"`);
    }
  } catch {
    // Silently fail
  }
}

/**
 * Create and start a Truffle node with sensible defaults.
 *
 * - Auto-resolves the sidecar binary path
 * - Auto-opens the Tailscale auth URL in the browser (configurable)
 * - Provides auth and peer lifecycle callbacks
 *
 * @example
 * ```ts
 * const node = await createMeshNode({
 *   name: 'my-app',
 *   onPeerChange: (event) => console.log('Peer event:', event),
 * });
 *
 * const peers = await node.getPeers();
 * await node.send('peer-id', 'chat', Buffer.from(JSON.stringify({ text: 'hello' })));
 * ```
 */
export async function createMeshNode(options: CreateMeshNodeOptions): Promise<NapiNode> {
  const {
    name,
    autoAuth = true,
    openUrl: customOpenUrl,
    onAuthRequired,
    onPeerChange,
    sidecarPath,
    stateDir,
    authKey,
    ephemeral,
    wsPort,
  } = options;

  const resolvedSidecarPath = sidecarPath ?? resolveSidecarPath();

  const node = new NapiNode();

  // IMPORTANT: install the auth-required callback BEFORE calling `start()`.
  // `start()` blocks waiting for Tailscale authentication to complete, so
  // an `onPeerChange` subscription installed afterwards misses the
  // `auth_required` event entirely and `start()` hangs until its internal
  // 5-minute timeout with "timed out waiting for authentication".
  //
  // `onAuthRequired` is a distinct, pre-start hook on NapiNode that bridges
  // to `NodeBuilder::build_with_auth_handler` under the hood.
  node.onAuthRequired((url: string) => {
    if (autoAuth) {
      const opener = customOpenUrl ?? defaultOpenUrl;
      opener(url);
    }
    onAuthRequired?.(url);
  });

  const config: NapiNodeConfig = {
    name,
    sidecarPath: resolvedSidecarPath,
    stateDir,
    authKey,
    ephemeral,
    wsPort,
  };

  await node.start(config);

  // Post-start: forward ongoing peer lifecycle events to the user callback.
  if (onPeerChange) {
    node.onPeerChange(onPeerChange);
  }

  return node;
}
