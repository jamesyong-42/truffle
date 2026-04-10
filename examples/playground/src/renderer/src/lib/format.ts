/**
 * Display helpers: relative timestamps, byte sizes, transfer speeds.
 * Pure functions with no React dependencies.
 */

export function formatRelativeTime(ts: number, now = Date.now()): string {
  const diff = Math.max(0, now - ts);
  const secs = Math.round(diff / 1000);
  if (secs < 5) return 'just now';
  if (secs < 60) return `${secs}s ago`;
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  return `${days}d ago`;
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '—';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let i = 0;
  let n = bytes;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i += 1;
  }
  const precision = i === 0 ? 0 : n < 10 ? 2 : n < 100 ? 1 : 0;
  return `${n.toFixed(precision)} ${units[i]}`;
}

export function formatSpeed(bytesPerSec: number): string {
  if (!Number.isFinite(bytesPerSec) || bytesPerSec <= 0) return '—';
  return `${formatBytes(bytesPerSec)}/s`;
}

export function formatElapsed(secs: number): string {
  if (!Number.isFinite(secs) || secs < 0) return '—';
  if (secs < 1) return `${Math.round(secs * 1000)}ms`;
  if (secs < 60) return `${secs.toFixed(1)}s`;
  const mins = Math.floor(secs / 60);
  const rest = Math.round(secs - mins * 60);
  return `${mins}m ${rest}s`;
}

export function shortHash(hash: string, len = 8): string {
  if (!hash) return '';
  return hash.slice(0, len) + '…';
}

export function formatClockTime(ts: number): string {
  const d = new Date(ts);
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  const ss = String(d.getSeconds()).padStart(2, '0');
  return `${hh}:${mm}:${ss}`;
}
