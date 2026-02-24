// Package tsnet wraps Tailscale's tsnet library for embedding.
package tsnet

import (
	"context"
	"fmt"
	"log"
	"net"
	"net/http"
	"strings"
	"sync"

	"tailscale.com/client/tailscale"
	"tailscale.com/tsnet"
)

// State represents the current state of the tsnet node
type State string

const (
	StateStopped  State = "stopped"
	StateStarting State = "starting"
	StateRunning  State = "running"
	StateStopping State = "stopping"
	StateError    State = "error"
)

// AuthCallback is called when Tailscale requires authentication
type AuthCallback func(authURL string)

// StatusCallback is called when node status changes
type StatusCallback func(state State, hostname, ip string, err error)

// Node wraps a tsnet.Server with lifecycle management
type Node struct {
	server    *tsnet.Server
	listeners []net.Listener // Multiple listeners for proxy support
	state     State
	hostname  string
	stateDir  string
	authKey   string

	mu         sync.RWMutex
	onAuth     AuthCallback
	onStatus   StatusCallback
	cancelFunc context.CancelFunc
}

// Config holds configuration for creating a new node
type Config struct {
	Hostname string
	StateDir string
	AuthKey  string
}

// NewNode creates a new tsnet node
func NewNode(cfg Config) *Node {
	return &Node{
		hostname: cfg.Hostname,
		stateDir: cfg.StateDir,
		authKey:  cfg.AuthKey,
		state:    StateStopped,
	}
}

// OnAuth sets the callback for when authentication is required
func (n *Node) OnAuth(cb AuthCallback) {
	n.mu.Lock()
	defer n.mu.Unlock()
	n.onAuth = cb
}

// OnStatus sets the callback for status changes
func (n *Node) OnStatus(cb StatusCallback) {
	n.mu.Lock()
	defer n.mu.Unlock()
	n.onStatus = cb
}

// Start initializes and starts the tsnet server
func (n *Node) Start(ctx context.Context) error {
	n.mu.Lock()
	if n.state == StateRunning || n.state == StateStarting {
		n.mu.Unlock()
		return fmt.Errorf("node already running or starting")
	}
	n.state = StateStarting
	n.mu.Unlock()

	n.notifyStatus(StateStarting, "", "", nil)

	// Create tsnet server
	n.server = &tsnet.Server{
		Hostname: n.hostname,
		Dir:      n.stateDir,
		Logf:     log.Printf,
	}

	if n.authKey != "" {
		n.server.AuthKey = n.authKey
	}

	// Start the server
	if err := n.server.Start(); err != nil {
		n.mu.Lock()
		n.state = StateError
		n.mu.Unlock()
		n.notifyStatus(StateError, "", "", err)
		return fmt.Errorf("failed to start tsnet server: %w", err)
	}

	// Get our Tailscale IP
	ctx2, cancel := context.WithCancel(ctx)
	n.mu.Lock()
	n.cancelFunc = cancel
	n.mu.Unlock()

	// Wait for tailscale to be up
	go func() {
		lc, err := n.server.LocalClient()
		if err != nil {
			log.Printf("Failed to get local client: %v", err)
			return
		}

		authURLSent := false

		// Poll for status until running
		for {
			select {
			case <-ctx2.Done():
				return
			default:
			}

			status, err := lc.StatusWithoutPeers(ctx2)
			if err != nil {
				log.Printf("Failed to get status: %v", err)
				continue
			}

			// Check for auth URL (may appear after initial status check)
			if !authURLSent && status.AuthURL != "" && n.onAuth != nil {
				log.Printf("Sending auth URL to Electron: %s", status.AuthURL)
				n.onAuth(status.AuthURL)
				authURLSent = true
			}

			if status.BackendState == "Running" {
				var ip string
				if len(status.TailscaleIPs) > 0 {
					ip = status.TailscaleIPs[0].String()
				}

				n.mu.Lock()
				n.state = StateRunning
				n.mu.Unlock()
				n.notifyStatus(StateRunning, n.hostname, ip, nil)
				return
			}
		}
	}()

	return nil
}

// Stop shuts down the tsnet server
func (n *Node) Stop() error {
	n.mu.Lock()
	if n.state == StateStopped || n.state == StateStopping {
		n.mu.Unlock()
		return nil
	}
	n.state = StateStopping
	if n.cancelFunc != nil {
		n.cancelFunc()
	}
	n.mu.Unlock()

	n.notifyStatus(StateStopping, "", "", nil)

	// Close all listeners
	n.mu.Lock()
	for _, ln := range n.listeners {
		if ln != nil {
			ln.Close()
		}
	}
	n.listeners = nil
	n.mu.Unlock()

	if n.server != nil {
		if err := n.server.Close(); err != nil {
			return fmt.Errorf("failed to close tsnet server: %w", err)
		}
		n.server = nil
	}

	n.mu.Lock()
	n.state = StateStopped
	n.mu.Unlock()
	n.notifyStatus(StateStopped, "", "", nil)

	return nil
}

// Listen creates a listener on the specified address
func (n *Node) Listen(network, addr string) (net.Listener, error) {
	if n.server == nil {
		return nil, fmt.Errorf("node not started")
	}

	ln, err := n.server.Listen(network, addr)
	if err != nil {
		return nil, err
	}

	n.mu.Lock()
	n.listeners = append(n.listeners, ln)
	n.mu.Unlock()

	return ln, nil
}

// ListenTLS creates a TLS listener with automatic Tailscale certificates
func (n *Node) ListenTLS(network, addr string) (net.Listener, error) {
	if n.server == nil {
		return nil, fmt.Errorf("node not started")
	}

	ln, err := n.server.ListenTLS(network, addr)
	if err != nil {
		return nil, err
	}

	n.mu.Lock()
	n.listeners = append(n.listeners, ln)
	n.mu.Unlock()

	return ln, nil
}

// Serve starts an HTTP server on the listener
func (n *Node) Serve(ln net.Listener, handler http.Handler) error {
	return http.Serve(ln, handler)
}

// State returns the current state
func (n *Node) State() State {
	n.mu.RLock()
	defer n.mu.RUnlock()
	return n.state
}

// GetStatus returns current status info
func (n *Node) GetStatus(ctx context.Context) (hostname, ip string, err error) {
	n.mu.RLock()
	state := n.state
	n.mu.RUnlock()

	if state != StateRunning || n.server == nil {
		return "", "", fmt.Errorf("node not running")
	}

	lc, err := n.server.LocalClient()
	if err != nil {
		return "", "", err
	}

	status, err := lc.StatusWithoutPeers(ctx)
	if err != nil {
		return "", "", err
	}

	hostname = n.hostname
	if len(status.TailscaleIPs) > 0 {
		ip = status.TailscaleIPs[0].String()
	}

	return hostname, ip, nil
}

// GetDNSName returns the full MagicDNS name for this node (e.g., "hostname.tailnet.ts.net")
func (n *Node) GetDNSName(ctx context.Context) (string, error) {
	n.mu.RLock()
	state := n.state
	n.mu.RUnlock()

	if state != StateRunning || n.server == nil {
		return "", fmt.Errorf("node not running")
	}

	lc, err := n.server.LocalClient()
	if err != nil {
		return "", err
	}

	status, err := lc.StatusWithoutPeers(ctx)
	if err != nil {
		return "", err
	}

	// Self.DNSName includes trailing dot, remove it
	dnsName := strings.TrimSuffix(status.Self.DNSName, ".")
	return dnsName, nil
}

// Server returns the underlying tsnet.Server (for advanced usage)
func (n *Node) Server() *tsnet.Server {
	return n.server
}

// LocalClient returns the Tailscale LocalClient for API access
func (n *Node) LocalClient() (*tailscale.LocalClient, error) {
	if n.server == nil {
		return nil, fmt.Errorf("node not started")
	}
	return n.server.LocalClient()
}

// Dial connects to an address on the Tailscale network
func (n *Node) Dial(ctx context.Context, network, addr string) (net.Conn, error) {
	if n.server == nil {
		return nil, fmt.Errorf("node not started")
	}
	return n.server.Dial(ctx, network, addr)
}

// PeerInfo represents a peer on the tailnet
type PeerInfo struct {
	ID           string
	Hostname     string
	DNSName      string
	TailscaleIPs []string
	Online       bool
	OS           string
}

// GetPeers returns all peers on the tailnet
func (n *Node) GetPeers(ctx context.Context) ([]PeerInfo, error) {
	return n.GetPeersFiltered(ctx, "")
}

// GetPeersFiltered returns peers on the tailnet, optionally filtered by hostname prefix
func (n *Node) GetPeersFiltered(ctx context.Context, hostnamePrefix string) ([]PeerInfo, error) {
	n.mu.RLock()
	state := n.state
	n.mu.RUnlock()

	if state != StateRunning || n.server == nil {
		return nil, fmt.Errorf("node not running")
	}

	lc, err := n.server.LocalClient()
	if err != nil {
		return nil, err
	}

	status, err := lc.Status(ctx)
	if err != nil {
		return nil, err
	}

	var peers []PeerInfo
	for _, peer := range status.Peer {
		// Filter by hostname prefix if specified
		if hostnamePrefix != "" && !strings.HasPrefix(peer.HostName, hostnamePrefix) {
			continue
		}

		var ips []string
		for _, ip := range peer.TailscaleIPs {
			ips = append(ips, ip.String())
		}

		// DNSName may have trailing dot, remove it for consistency
		dnsName := strings.TrimSuffix(peer.DNSName, ".")
		peers = append(peers, PeerInfo{
			ID:           string(peer.ID),
			Hostname:     peer.HostName,
			DNSName:      dnsName,
			TailscaleIPs: ips,
			Online:       peer.Online,
			OS:           peer.OS,
		})
	}

	return peers, nil
}

func (n *Node) notifyStatus(state State, hostname, ip string, err error) {
	n.mu.RLock()
	cb := n.onStatus
	n.mu.RUnlock()

	if cb != nil {
		cb(state, hostname, ip, err)
	}
}
