/**
 * Resolve the sidecar binary path for the current platform.
 * @returns Absolute path to the sidecar binary.
 * @throws If the binary cannot be found.
 */
export declare function resolveSidecarPath(): string;

/**
 * Open a URL in the system's default browser.
 * Works cross-platform (macOS, Linux, Windows).
 */
export declare function openUrl(url: string): void;
