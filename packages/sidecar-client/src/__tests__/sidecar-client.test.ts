import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { EventEmitter } from 'events';
import { SidecarClient } from '../index.js';

// We need to mock readline.createInterface because our mock stdout
// is not a real stream (no resume/pause methods).
// We'll capture the mock rl emitter so tests can emit 'line' events.
let mockRlEmitter: EventEmitter;

vi.mock('readline', () => ({
  createInterface: vi.fn(() => {
    mockRlEmitter = new EventEmitter();
    (mockRlEmitter as any).close = vi.fn();
    return mockRlEmitter;
  }),
}));

// Mock child_process.spawn
vi.mock('child_process', () => {
  return {
    spawn: vi.fn(),
  };
});

import { spawn } from 'child_process';

function createMockProcess() {
  const stdin = {
    write: vi.fn(),
  };
  const stdoutEmitter = new EventEmitter();
  const stdout = Object.assign(stdoutEmitter, {
    pipe: vi.fn(),
  });
  const stderrEmitter = new EventEmitter();
  const stderr = Object.assign(stderrEmitter, {
    pipe: vi.fn(),
  });
  const processEmitter = new EventEmitter();
  const proc = Object.assign(processEmitter, {
    stdin,
    stdout,
    stderr,
    kill: vi.fn(),
    pid: 12345,
  });

  return proc;
}

describe('@vibecook/truffle-sidecar-client', () => {
  let client: SidecarClient;
  let mockProcess: ReturnType<typeof createMockProcess>;

  beforeEach(() => {
    client = new SidecarClient();
    mockProcess = createMockProcess();
    vi.mocked(spawn).mockReturnValue(mockProcess as any);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe('command serialization', () => {
    it('sends start command with correct JSON', async () => {
      const startPromise = client.start({
        sidecarPath: '/path/to/sidecar',
        hostname: 'test-host',
        stateDir: '/state',
        authKey: 'tskey-123',
        hostnamePrefix: 'myapp',
      });

      // Simulate successful start
      const line = JSON.stringify({
        event: 'tsnet:status',
        data: { state: 'running', hostname: 'test-host', tailscaleIP: '100.64.0.1' },
      });
      mockRlEmitter.emit('line', line);

      await startPromise;

      expect(mockProcess.stdin.write).toHaveBeenCalledWith(
        expect.stringContaining('"command":"tsnet:start"')
      );
      const call = mockProcess.stdin.write.mock.calls[0][0];
      const parsed = JSON.parse(call.replace('\n', ''));
      expect(parsed.command).toBe('tsnet:start');
      expect(parsed.data.hostname).toBe('test-host');
      expect(parsed.data.stateDir).toBe('/state');
      expect(parsed.data.authKey).toBe('tskey-123');
      expect(parsed.data.hostnamePrefix).toBe('myapp');
    });
  });

  describe('event parsing', () => {
    it('emits statusChanged on status event', async () => {
      const startPromise = client.start({
        sidecarPath: '/path/to/sidecar',
        hostname: 'test-host',
        stateDir: '/state',
      });

      const statusHandler = vi.fn();
      client.on('statusChanged', statusHandler);

      const line = JSON.stringify({
        event: 'tsnet:status',
        data: {
          state: 'running',
          hostname: 'test-host',
          dnsName: 'test-host.tailnet.ts.net',
          tailscaleIP: '100.64.0.1',
        },
      });
      mockRlEmitter.emit('line', line);

      await startPromise;

      // First call is 'starting', second is 'running'
      expect(statusHandler).toHaveBeenCalledWith(
        expect.objectContaining({ state: 'running' })
      );
    });

    it('emits authRequired event', async () => {
      const startPromise = client.start({
        sidecarPath: '/path/to/sidecar',
        hostname: 'test-host',
        stateDir: '/state',
      });

      const authHandler = vi.fn();
      client.on('authRequired', authHandler);

      mockRlEmitter.emit('line', JSON.stringify({
        event: 'tsnet:authRequired',
        data: { authUrl: 'https://login.tailscale.com/...' },
      }));

      expect(authHandler).toHaveBeenCalledWith('https://login.tailscale.com/...');

      // Complete startup to clean up
      mockRlEmitter.emit('line', JSON.stringify({
        event: 'tsnet:status',
        data: { state: 'running', tailscaleIP: '100.64.0.1' },
      }));
      await startPromise;
    });

    it('emits peer events', async () => {
      const startPromise = client.start({
        sidecarPath: '/path/to/sidecar',
        hostname: 'test-host',
        stateDir: '/state',
      });

      mockRlEmitter.emit('line', JSON.stringify({
        event: 'tsnet:status',
        data: { state: 'running', tailscaleIP: '100.64.0.1' },
      }));
      await startPromise;

      const peersHandler = vi.fn();
      client.on('tailnetPeers', peersHandler);

      mockRlEmitter.emit('line', JSON.stringify({
        event: 'tsnet:peers',
        data: {
          peers: [
            {
              id: 'p1',
              hostname: 'myapp-desktop-abc',
              dnsName: 'myapp-desktop-abc.tailnet.ts.net',
              tailscaleIPs: ['100.64.0.2'],
              online: true,
            },
          ],
        },
      }));

      expect(peersHandler).toHaveBeenCalledWith([
        expect.objectContaining({ id: 'p1', hostname: 'myapp-desktop-abc' }),
      ]);
    });

    it('emits WebSocket events', async () => {
      const startPromise = client.start({
        sidecarPath: '/path/to/sidecar',
        hostname: 'test-host',
        stateDir: '/state',
      });

      mockRlEmitter.emit('line', JSON.stringify({
        event: 'tsnet:status',
        data: { state: 'running', tailscaleIP: '100.64.0.1' },
      }));
      await startPromise;

      const wsConnectHandler = vi.fn();
      const wsMessageHandler = vi.fn();
      const wsDisconnectHandler = vi.fn();

      client.on('wsConnect', wsConnectHandler);
      client.on('wsMessage', wsMessageHandler);
      client.on('wsDisconnect', wsDisconnectHandler);

      mockRlEmitter.emit('line', JSON.stringify({
        event: 'tsnet:wsConnect',
        data: { connectionId: 'conn-1', remoteAddr: '100.64.0.2:443' },
      }));

      expect(wsConnectHandler).toHaveBeenCalledWith('conn-1', '100.64.0.2:443');

      mockRlEmitter.emit('line', JSON.stringify({
        event: 'tsnet:wsMessage',
        data: { connectionId: 'conn-1', data: '{"test":"data"}' },
      }));

      expect(wsMessageHandler).toHaveBeenCalledWith('conn-1', '{"test":"data"}');

      mockRlEmitter.emit('line', JSON.stringify({
        event: 'tsnet:wsDisconnect',
        data: { connectionId: 'conn-1', reason: 'closed' },
      }));

      expect(wsDisconnectHandler).toHaveBeenCalledWith('conn-1', 'closed');
    });

    it('emits dial events', async () => {
      const startPromise = client.start({
        sidecarPath: '/path/to/sidecar',
        hostname: 'test-host',
        stateDir: '/state',
      });

      mockRlEmitter.emit('line', JSON.stringify({
        event: 'tsnet:status',
        data: { state: 'running', tailscaleIP: '100.64.0.1' },
      }));
      await startPromise;

      const dialConnectedHandler = vi.fn();
      const dialMessageHandler = vi.fn();
      const dialDisconnectHandler = vi.fn();
      const dialErrorHandler = vi.fn();

      client.on('dialConnected', dialConnectedHandler);
      client.on('dialMessage', dialMessageHandler);
      client.on('dialDisconnect', dialDisconnectHandler);
      client.on('dialError', dialErrorHandler);

      mockRlEmitter.emit('line', JSON.stringify({
        event: 'tsnet:dialConnected',
        data: { deviceId: 'dev-2', remoteAddr: '100.64.0.2:443' },
      }));

      expect(dialConnectedHandler).toHaveBeenCalledWith('dev-2', '100.64.0.2:443');

      mockRlEmitter.emit('line', JSON.stringify({
        event: 'tsnet:dialMessage',
        data: { deviceId: 'dev-2', data: 'hello' },
      }));

      expect(dialMessageHandler).toHaveBeenCalledWith('dev-2', 'hello');

      mockRlEmitter.emit('line', JSON.stringify({
        event: 'tsnet:dialError',
        data: { deviceId: 'dev-2', error: 'connection refused' },
      }));

      expect(dialErrorHandler).toHaveBeenCalledWith('dev-2', 'connection refused');
    });
  });

  describe('getStatus', () => {
    it('returns stopped initially', () => {
      const status = client.getStatus();
      expect(status.state).toBe('stopped');
    });

    it('returns running status copy', async () => {
      const startPromise = client.start({
        sidecarPath: '/path/to/sidecar',
        hostname: 'test-host',
        stateDir: '/state',
      });

      mockRlEmitter.emit('line', JSON.stringify({
        event: 'tsnet:status',
        data: { state: 'running', hostname: 'test-host', tailscaleIP: '100.64.0.1' },
      }));

      await startPromise;

      const status = client.getStatus();
      expect(status.state).toBe('running');
      expect(status.hostname).toBe('test-host');
    });
  });

  describe('isRunning', () => {
    it('returns false when stopped', () => {
      expect(client.isRunning()).toBe(false);
    });
  });

  describe('sendToConnection', () => {
    it('does nothing when not running', () => {
      client.sendToConnection('conn-1', 'data');
      expect(mockProcess.stdin.write).not.toHaveBeenCalled();
    });
  });

  describe('dial commands', () => {
    it('sends dial command with correct params', async () => {
      const startPromise = client.start({
        sidecarPath: '/path/to/sidecar',
        hostname: 'test-host',
        stateDir: '/state',
      });

      mockRlEmitter.emit('line', JSON.stringify({
        event: 'tsnet:status',
        data: { state: 'running', tailscaleIP: '100.64.0.1' },
      }));

      await startPromise;

      client.dial('dev-2', 'myapp-desktop-dev-2', 'myapp-desktop-dev-2.tailnet.ts.net', 443);

      const lastCall = mockProcess.stdin.write.mock.calls.at(-1)![0];
      const parsed = JSON.parse(lastCall.replace('\n', ''));
      expect(parsed.command).toBe('tsnet:dial');
      expect(parsed.data.deviceId).toBe('dev-2');
      expect(parsed.data.hostname).toBe('myapp-desktop-dev-2');
    });
  });
});
