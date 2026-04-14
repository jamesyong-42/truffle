# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.4.x   | :white_check_mark: |
| < 0.4   | :x:                |

## Reporting a Vulnerability

If you discover a security vulnerability in Truffle, please report it responsibly.

**Do NOT open a public GitHub issue for security vulnerabilities.**

Instead, please email **security@vibecook.com** with:

1. A description of the vulnerability
2. Steps to reproduce the issue
3. The potential impact
4. Any suggested fixes (optional)

You should receive a response within **48 hours** acknowledging receipt. We will work with you to understand the issue and coordinate a fix before any public disclosure.

## Scope

The following are in scope for security reports:

- **truffle-core**: Mesh networking protocol, peer authentication, message encryption
- **truffle-cli**: CLI tool and daemon
- **truffle-sidecar**: Sidecar binary download and verification
- **@vibecook/truffle** (npm): Node.js bindings via NAPI-RS
- **truffle-tauri-plugin**: Tauri v2 desktop plugin

## Security Best Practices

Truffle relies on [Tailscale](https://tailscale.com) for transport-layer security (WireGuard tunnels). The Truffle layer adds:

- Namespace-isolated message routing
- Peer identity verification via Tailscale node keys
- File transfer with SHA-256 integrity verification
