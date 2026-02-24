/**
 * SidecarClient - Manages the Go sidecar process
 *
 * Spawns and manages the Go sidecar binary that embeds Tailscale.
 * Communicates via JSON over stdin/stdout.
 *
 * No singletons, no Electron deps - all paths provided by consumer.
 */

import { EventEmitter } from 'events';
import { type ChildProcess, spawn } from 'child_process';
import { createInterface, type Interface } from 'readline';
import { createLogger } from '@vibecook/truffle-types';
import type {
  SidecarConfig,
  SidecarStatus,
  SidecarState,
  SidecarServiceEvents,
  TailnetPeer,
  Logger,
} from '@vibecook/truffle-types';

// ═══════════════════════════════════════════════════════════════════════════
// CONSTANTS
// ═══════════════════════════════════════════════════════════════════════════

const STARTUP_TIMEOUT_MS = 30000;
const SHUTDOWN_TIMEOUT_MS = 5000;

// ═══════════════════════════════════════════════════════════════════════════
// IPC TYPES
// ═══════════════════════════════════════════════════════════════════════════

interface IPCCommand {
  command: string;
  data?: unknown;
}

interface IPCEvent {
  event: string;
  data?: unknown;
}

interface StatusEventData {
  state: SidecarState;
  hostname?: string;
  dnsName?: string;
  tailscaleIP?: string;
  error?: string;
}

interface WsConnectEventData {
  connectionId: string;
  remoteAddr: string;
}

interface WsMessageEventData {
  connectionId: string;
  data: string;
}

interface WsDisconnectEventData {
  connectionId: string;
  reason?: string;
}

interface AuthRequiredEventData {
  authUrl: string;
}

interface ErrorEventData {
  code: string;
  message: string;
}

interface PeersEventData {
  peers: TailnetPeer[];
}

interface DialConnectedEventData {
  deviceId: string;
  remoteAddr: string;
}

interface DialMessageEventData {
  deviceId: string;
  data: string;
}

interface DialDisconnectEventData {
  deviceId: string;
  reason?: string;
}

interface DialErrorEventData {
  deviceId: string;
  error: string;
}

// ═══════════════════════════════════════════════════════════════════════════
// INTERFACE
// ═══════════════════════════════════════════════════════════════════════════

export interface ISidecarClient extends EventEmitter {
  start(config: SidecarConfig): Promise<void>;
  stop(): Promise<void>;
  getStatus(): SidecarStatus;
  isRunning(): boolean;

  sendToConnection(connectionId: string, data: string): void;
  dial(deviceId: string, hostname: string, dnsName?: string, port?: number): void;
  dialClose(deviceId: string): void;
  dialSend(deviceId: string, data: string): void;
  getTailnetPeers(): void;

  on<K extends keyof SidecarServiceEvents>(event: K, listener: SidecarServiceEvents[K]): this;
  off<K extends keyof SidecarServiceEvents>(event: K, listener: SidecarServiceEvents[K]): this;
  emit<K extends keyof SidecarServiceEvents>(event: K, ...args: Parameters<SidecarServiceEvents[K]>): boolean;
}

// ═══════════════════════════════════════════════════════════════════════════
// IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════════════

export class SidecarClient extends EventEmitter implements ISidecarClient {
  private process: ChildProcess | null = null;
  private readline: Interface | null = null;
  private status: SidecarStatus = { state: 'stopped' };
  private readonly log: Logger;

  constructor(logger?: Logger) {
    super();
    this.log = logger ?? createLogger('SidecarClient');
  }

  // ─────────────────────────────────────────────────────────────────────────
  // LIFECYCLE
  // ─────────────────────────────────────────────────────────────────────────

  async start(config: SidecarConfig): Promise<void> {
    if (this.process) {
      throw new Error('Sidecar already running');
    }

    this.status.state = 'starting';
    this.emit('statusChanged', this.status);

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.cleanup();
        reject(new Error('Sidecar startup timeout'));
      }, STARTUP_TIMEOUT_MS);

      try {
        this.process = spawn(config.sidecarPath, [], {
          stdio: ['pipe', 'pipe', 'pipe'],
          env: { ...process.env },
        });

        if (!this.process.stdout || !this.process.stdin) {
          throw new Error('Failed to create stdio pipes');
        }

        this.readline = createInterface({
          input: this.process.stdout,
          crlfDelay: Infinity,
        });

        this.readline.on('line', (line) => {
          this.handleEvent(line);
        });

        this.process.stderr?.on('data', (data) => {
          this.log.error('stderr:', data.toString());
        });

        this.process.on('exit', (code, signal) => {
          this.log.info(`Process exited: code=${code} signal=${signal}`);
          this.cleanup();
          this.status.state = 'stopped';
          this.emit('statusChanged', this.status);
        });

        this.process.on('error', (error) => {
          this.log.error('Process error:', error);
          this.emit('error', error);
          reject(error);
        });

        const onStatus = (status: SidecarStatus) => {
          if (status.state === 'running') {
            clearTimeout(timeout);
            this.off('statusChanged', onStatus);
            resolve();
          } else if (status.state === 'error') {
            clearTimeout(timeout);
            this.off('statusChanged', onStatus);
            reject(new Error(status.error || 'Sidecar startup failed'));
          }
        };
        this.on('statusChanged', onStatus);

        // Send start command with configurable hostnamePrefix
        this.sendCommand('tsnet:start', {
          hostname: config.hostname,
          stateDir: config.stateDir,
          authKey: config.authKey,
          staticPath: config.staticPath,
          hostnamePrefix: config.hostnamePrefix,
        });
      } catch (error) {
        clearTimeout(timeout);
        this.cleanup();
        reject(error);
      }
    });
  }

  async stop(): Promise<void> {
    if (!this.process) {
      return;
    }

    this.status.state = 'stopping';
    this.emit('statusChanged', this.status);

    return new Promise((resolve) => {
      const timeout = setTimeout(() => {
        this.process?.kill('SIGKILL');
        this.cleanup();
        resolve();
      }, SHUTDOWN_TIMEOUT_MS);

      const onExit = () => {
        clearTimeout(timeout);
        this.process?.off('exit', onExit);
        resolve();
      };

      this.process?.on('exit', onExit);
      this.sendCommand('tsnet:stop', {});
    });
  }

  getStatus(): SidecarStatus {
    return { ...this.status };
  }

  isRunning(): boolean {
    return this.status.state === 'running';
  }

  // ─────────────────────────────────────────────────────────────────────────
  // WEBSOCKET MESSAGING
  // ─────────────────────────────────────────────────────────────────────────

  sendToConnection(connectionId: string, data: string): void {
    if (!this.isRunning()) return;
    this.sendCommand('tsnet:wsMessage', { connectionId, data });
  }

  getTailnetPeers(): void {
    if (!this.isRunning()) return;
    this.sendCommand('tsnet:getPeers', {});
  }

  // ─────────────────────────────────────────────────────────────────────────
  // DIAL (OUTGOING CONNECTIONS)
  // ─────────────────────────────────────────────────────────────────────────

  dial(deviceId: string, hostname: string, dnsName?: string, port: number = 443): void {
    if (!this.isRunning()) {
      this.log.warn('Cannot dial: not running');
      return;
    }
    this.log.info(`Dialing ${hostname}:${port} (deviceId: ${deviceId})`);
    this.sendCommand('tsnet:dial', {
      deviceId,
      hostname,
      dnsName: dnsName ?? '',
      port,
    });
  }

  dialClose(deviceId: string): void {
    if (!this.isRunning()) return;
    this.sendCommand('tsnet:dialClose', { deviceId });
  }

  dialSend(deviceId: string, data: string): void {
    if (!this.isRunning()) return;
    this.sendCommand('tsnet:dialMessage', { deviceId, data });
  }

  // ─────────────────────────────────────────────────────────────────────────
  // PRIVATE - IPC
  // ─────────────────────────────────────────────────────────────────────────

  private sendCommand(command: string, data: unknown): void {
    if (!this.process?.stdin) return;
    const cmd: IPCCommand = { command, data };
    this.process.stdin.write(JSON.stringify(cmd) + '\n');
  }

  private handleEvent(line: string): void {
    try {
      const event = JSON.parse(line) as IPCEvent;
      this.routeEvent(event);
    } catch (error) {
      this.log.error('Failed to parse event:', error);
    }
  }

  private routeEvent(event: IPCEvent): void {
    switch (event.event) {
      case 'tsnet:status':
        this.handleStatusEvent(event.data as StatusEventData);
        break;
      case 'tsnet:authRequired':
        this.emit('authRequired', (event.data as AuthRequiredEventData).authUrl);
        break;
      case 'tsnet:peers':
        this.emit('tailnetPeers', (event.data as PeersEventData).peers);
        break;
      case 'tsnet:wsConnect': {
        const d = event.data as WsConnectEventData;
        this.emit('wsConnect', d.connectionId, d.remoteAddr);
        break;
      }
      case 'tsnet:wsMessage': {
        const d = event.data as WsMessageEventData;
        this.emit('wsMessage', d.connectionId, d.data);
        break;
      }
      case 'tsnet:wsDisconnect': {
        const d = event.data as WsDisconnectEventData;
        this.emit('wsDisconnect', d.connectionId, d.reason);
        break;
      }
      case 'tsnet:error': {
        const d = event.data as ErrorEventData;
        this.emit('error', new Error(`${d.code}: ${d.message}`));
        break;
      }
      case 'tsnet:dialConnected': {
        const d = event.data as DialConnectedEventData;
        this.emit('dialConnected', d.deviceId, d.remoteAddr);
        break;
      }
      case 'tsnet:dialMessage': {
        const d = event.data as DialMessageEventData;
        this.emit('dialMessage', d.deviceId, d.data);
        break;
      }
      case 'tsnet:dialDisconnect': {
        const d = event.data as DialDisconnectEventData;
        this.emit('dialDisconnect', d.deviceId, d.reason);
        break;
      }
      case 'tsnet:dialError': {
        const d = event.data as DialErrorEventData;
        this.emit('dialError', d.deviceId, d.error);
        break;
      }
      default:
        this.log.warn(`Unknown event: ${event.event}`);
    }
  }

  private handleStatusEvent(data: StatusEventData): void {
    this.status = {
      state: data.state,
      hostname: data.hostname,
      dnsName: data.dnsName,
      tailscaleIP: data.tailscaleIP,
      error: data.error,
    };
    this.emit('statusChanged', this.status);
  }

  private cleanup(): void {
    this.readline?.close();
    this.readline = null;
    this.process = null;
  }
}
