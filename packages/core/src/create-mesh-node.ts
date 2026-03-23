import { execSync } from 'node:child_process';
import {
  NapiMeshNode,
  type NapiMeshNodeConfig,
  type NapiMeshEvent,
} from '@vibecook/truffle-native';
import { resolveSidecarPath } from './sidecar.js';

export interface CreateMeshNodeOptions {
  /** Unique device identifier. */
  deviceId: string;
  /** User-visible device name. */
  deviceName: string;
  /** Device type (e.g., "desktop", "mobile", "server"). */
  deviceType: string;
  /** Hostname prefix for Tailscale (default: "truffle"). */
  hostnamePrefix?: string;
  /** Override sidecar path. If omitted, auto-resolved. */
  sidecarPath?: string;
  /** State directory for Tailscale. */
  stateDir?: string;
  /** Tailscale auth key (for headless/CI setups). */
  authKey?: string;
  /** Whether this device prefers to be primary. */
  preferPrimary?: boolean;
  /** Device capabilities. */
  capabilities?: string[];
  /** Timing configuration. */
  timing?: NapiMeshNodeConfig['timing'];
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
  /** Called when Tailscale auth completes. */
  onAuthComplete?: () => void;
  /** Called for all mesh events (devices, election, messages, etc.). */
  onEvent?: (event: NapiMeshEvent) => void;
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
 * Create a MeshNode with sensible defaults.
 *
 * - Auto-resolves the sidecar binary path
 * - Auto-opens the Tailscale auth URL in the browser (configurable)
 * - Provides auth lifecycle callbacks
 *
 * @example
 * ```ts
 * const node = createMeshNode({
 *   deviceId: 'my-app',
 *   deviceName: 'My App',
 *   deviceType: 'desktop',
 *   onAuthRequired: (url) => console.log('Auth URL:', url),
 *   onEvent: (event) => console.log('Event:', event),
 * });
 * await node.start();
 * ```
 */
export function createMeshNode(options: CreateMeshNodeOptions): NapiMeshNode {
  const {
    autoAuth = true,
    openUrl: customOpenUrl,
    onAuthRequired,
    onAuthComplete,
    onEvent,
    hostnamePrefix = 'truffle',
    sidecarPath,
    ...rest
  } = options;

  const resolvedSidecarPath = sidecarPath ?? resolveSidecarPath();

  const node = new NapiMeshNode({
    ...rest,
    hostnamePrefix,
    sidecarPath: resolvedSidecarPath,
  });

  node.onEvent((_err: null | Error, event: NapiMeshEvent) => {
    if (_err) return;

    if (event.eventType === 'authRequired' && event.payload) {
      const url =
        typeof event.payload === 'string'
          ? event.payload
          : String(event.payload);

      if (autoAuth) {
        const opener = customOpenUrl ?? defaultOpenUrl;
        opener(url);
      }

      onAuthRequired?.(url);
    }

    if (event.eventType === 'authComplete') {
      onAuthComplete?.();
    }

    onEvent?.(event);
  });

  return node;
}
