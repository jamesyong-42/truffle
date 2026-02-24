/**
 * PrimaryElection - Handles primary device election for STAR topology
 *
 * Election Logic:
 * 1. User-designated primary wins (preferPrimary setting)
 * 2. Longest uptime wins
 * 3. Alphabetically lowest device ID as tiebreaker
 */

import { TypedEventEmitter, createMeshMessage, createLogger } from '@vibecook/truffle-types';
import type {
  MeshMessage,
  ElectionCandidatePayload,
  ElectionResultPayload,
  Logger,
} from '@vibecook/truffle-types';

// ═══════════════════════════════════════════════════════════════════════════
// CONSTANTS
// ═══════════════════════════════════════════════════════════════════════════

const DEFAULT_ELECTION_TIMEOUT_MS = 3000;
const DEFAULT_PRIMARY_LOSS_GRACE_PERIOD_MS = 5000;

// ═══════════════════════════════════════════════════════════════════════════
// TYPES
// ═══════════════════════════════════════════════════════════════════════════

export interface ElectionTimingConfig {
  electionTimeoutMs?: number;
  primaryLossGraceMs?: number;
}

export interface ElectionConfig {
  deviceId: string;
  startedAt: number;
  preferPrimary: boolean;
}

export interface ElectionState {
  phase: 'idle' | 'waiting' | 'collecting' | 'decided';
  primaryId: string | null;
  candidates: Map<string, ElectionCandidatePayload>;
  timeoutHandle: ReturnType<typeof setTimeout> | null;
  gracePeriodHandle: ReturnType<typeof setTimeout> | null;
}

export interface PrimaryElectionEvents {
  electionStarted: () => void;
  primaryElected: (deviceId: string, isLocal: boolean) => void;
  primaryLost: (previousPrimaryId: string) => void;
  broadcast: (message: MeshMessage) => void;
}

// ═══════════════════════════════════════════════════════════════════════════
// IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════════════

export class PrimaryElection extends TypedEventEmitter<PrimaryElectionEvents> {
  private config: ElectionConfig | null = null;
  private state: ElectionState = {
    phase: 'idle',
    primaryId: null,
    candidates: new Map(),
    timeoutHandle: null,
    gracePeriodHandle: null,
  };
  private readonly log: Logger;
  private readonly electionTimeoutMs: number;
  private readonly primaryLossGraceMs: number;

  constructor(logger?: Logger, timing?: ElectionTimingConfig) {
    super();
    this.log = logger ?? createLogger('PrimaryElection');
    this.electionTimeoutMs = timing?.electionTimeoutMs ?? DEFAULT_ELECTION_TIMEOUT_MS;
    this.primaryLossGraceMs = timing?.primaryLossGraceMs ?? DEFAULT_PRIMARY_LOSS_GRACE_PERIOD_MS;
  }

  // ─────────────────────────────────────────────────────────────────────────
  // CONFIGURATION
  // ─────────────────────────────────────────────────────────────────────────

  configure(config: ElectionConfig): void {
    this.config = config;
    this.log.info(`Configured for device ${config.deviceId}`);
  }

  // ─────────────────────────────────────────────────────────────────────────
  // STATE
  // ─────────────────────────────────────────────────────────────────────────

  isPrimary(): boolean {
    return this.state.primaryId === this.config?.deviceId;
  }

  getPrimaryId(): string | null {
    return this.state.primaryId;
  }

  getPhase(): ElectionState['phase'] {
    return this.state.phase;
  }

  setPrimary(deviceId: string): void {
    this.state.primaryId = deviceId;
    this.state.phase = 'idle';
    this.log.info(`Primary set to ${deviceId}`);
  }

  // ─────────────────────────────────────────────────────────────────────────
  // ELECTION TRIGGERS
  // ─────────────────────────────────────────────────────────────────────────

  handlePrimaryLost(previousPrimaryId: string): void {
    if (!this.config) return;

    this.log.info(`Primary lost: ${previousPrimaryId}`);
    this.state.primaryId = null;
    this.emit('primaryLost', previousPrimaryId);

    this.clearGracePeriod();
    this.state.phase = 'waiting';

    this.state.gracePeriodHandle = setTimeout(() => {
      this.log.info('Grace period expired, starting election');
      this.startElection();
    }, this.primaryLossGraceMs);
  }

  handleNoPrimaryOnStartup(): void {
    if (!this.config) return;
    this.log.info('No primary detected on startup');
    this.startElection();
  }

  // ─────────────────────────────────────────────────────────────────────────
  // ELECTION PROCESS
  // ─────────────────────────────────────────────────────────────────────────

  private startElection(): void {
    if (!this.config) return;
    if (this.state.phase === 'collecting') {
      this.log.info('Election already in progress');
      return;
    }

    this.log.info('Starting election');
    this.state.phase = 'collecting';
    this.state.candidates.clear();
    this.clearTimeout();

    const myCandidate: ElectionCandidatePayload = {
      deviceId: this.config.deviceId,
      uptime: Date.now() - this.config.startedAt,
      userDesignated: this.config.preferPrimary,
    };
    this.state.candidates.set(this.config.deviceId, myCandidate);

    const startMessage = createMeshMessage('election:start', this.config.deviceId, {});
    this.emit('broadcast', startMessage);

    const candidateMessage = createMeshMessage(
      'election:candidate',
      this.config.deviceId,
      myCandidate,
    );
    this.emit('broadcast', candidateMessage);

    this.emit('electionStarted');

    this.state.timeoutHandle = setTimeout(() => {
      this.decideElection();
    }, this.electionTimeoutMs);
  }

  private decideElection(): void {
    if (!this.config) return;

    this.log.info(`Deciding election with ${this.state.candidates.size} candidates`);

    const winner = this.selectWinner();
    if (!winner) {
      this.log.info('No candidates, becoming primary by default');
      this.becomePrimary();
      return;
    }

    this.log.info(`Winner: ${winner.deviceId}`);

    if (winner.deviceId === this.config.deviceId) {
      this.becomePrimary();
    } else {
      this.state.primaryId = winner.deviceId;
      this.state.phase = 'decided';
      this.emit('primaryElected', winner.deviceId, false);
    }
  }

  private selectWinner(): ElectionCandidatePayload | null {
    const candidates = Array.from(this.state.candidates.values());
    if (candidates.length === 0) return null;

    candidates.sort((a, b) => {
      if (a.userDesignated !== b.userDesignated) {
        return a.userDesignated ? -1 : 1;
      }
      if (a.uptime !== b.uptime) {
        return b.uptime - a.uptime;
      }
      return a.deviceId.localeCompare(b.deviceId);
    });

    return candidates[0];
  }

  private becomePrimary(): void {
    if (!this.config) return;

    this.log.info('Becoming primary');
    this.state.primaryId = this.config.deviceId;
    this.state.phase = 'decided';

    const resultPayload: ElectionResultPayload = {
      newPrimaryId: this.config.deviceId,
      reason: 'election',
    };
    const resultMessage = createMeshMessage('election:result', this.config.deviceId, resultPayload);
    this.emit('broadcast', resultMessage);
    this.emit('primaryElected', this.config.deviceId, true);
  }

  // ─────────────────────────────────────────────────────────────────────────
  // MESSAGE HANDLING
  // ─────────────────────────────────────────────────────────────────────────

  handleElectionStart(from: string): void {
    if (!this.config) return;

    this.log.info(`Received election:start from ${from}`);

    if (this.state.phase !== 'collecting') {
      this.state.phase = 'collecting';
      this.state.candidates.clear();
      this.clearTimeout();

      const myCandidate: ElectionCandidatePayload = {
        deviceId: this.config.deviceId,
        uptime: Date.now() - this.config.startedAt,
        userDesignated: this.config.preferPrimary,
      };
      this.state.candidates.set(this.config.deviceId, myCandidate);

      const candidateMessage = createMeshMessage(
        'election:candidate',
        this.config.deviceId,
        myCandidate,
      );
      this.emit('broadcast', candidateMessage);

      this.state.timeoutHandle = setTimeout(() => {
        this.decideElection();
      }, this.electionTimeoutMs);
    }
  }

  handleElectionCandidate(from: string, payload: ElectionCandidatePayload): void {
    if (!this.config) return;
    this.log.info(
      `Received candidacy from ${from}: uptime=${payload.uptime}, designated=${payload.userDesignated}`,
    );
    this.state.candidates.set(payload.deviceId, payload);
  }

  handleElectionResult(from: string, payload: ElectionResultPayload): void {
    if (!this.config) return;

    this.log.info(`Received election result from ${from}: winner=${payload.newPrimaryId}`);

    this.clearTimeout();
    this.clearGracePeriod();

    this.state.primaryId = payload.newPrimaryId;
    this.state.phase = 'decided';
    this.state.candidates.clear();

    const isLocal = payload.newPrimaryId === this.config.deviceId;
    this.emit('primaryElected', payload.newPrimaryId, isLocal);
  }

  // ─────────────────────────────────────────────────────────────────────────
  // CLEANUP
  // ─────────────────────────────────────────────────────────────────────────

  private clearTimeout(): void {
    if (this.state.timeoutHandle) {
      clearTimeout(this.state.timeoutHandle);
      this.state.timeoutHandle = null;
    }
  }

  private clearGracePeriod(): void {
    if (this.state.gracePeriodHandle) {
      clearTimeout(this.state.gracePeriodHandle);
      this.state.gracePeriodHandle = null;
    }
  }

  reset(): void {
    this.clearTimeout();
    this.clearGracePeriod();
    this.state = {
      phase: 'idle',
      primaryId: null,
      candidates: new Map(),
      timeoutHandle: null,
      gracePeriodHandle: null,
    };
  }
}
