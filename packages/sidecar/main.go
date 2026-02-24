// tsnet-sidecar is a Go binary that embeds Tailscale for mesh networking.
// It communicates with the Electron host via JSON over stdin/stdout.
//
// This version operates on private tailnet only - no Funnel/public access.
package main

import (
	"context"
	"encoding/json"
	"log"
	"os"
	"os/signal"
	"sync"
	"syscall"

	"github.com/jamesyong-42/claude-code-on-the-go/tsnet-sidecar/internal/ipc"
	"github.com/jamesyong-42/claude-code-on-the-go/tsnet-sidecar/internal/server"
	"github.com/jamesyong-42/claude-code-on-the-go/tsnet-sidecar/internal/tsnet"
)

// App holds the application state
type App struct {
	protocol     *ipc.Protocol
	node         *tsnet.Node
	server       *server.Server
	dialer       *server.Dialer
	proxyManager *server.ProxyManager
	mu           sync.RWMutex
	hostname     string
	ip           string
	dnsName      string // Full MagicDNS name (e.g., "hostname.tailnet.ts.net")
	vapidKey     string
	ctx          context.Context
	cancel       context.CancelFunc
}

func main() {
	log.SetOutput(os.Stderr) // Log to stderr, stdout is for IPC

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	app := &App{
		protocol: ipc.NewProtocol(),
		ctx:      ctx,
		cancel:   cancel,
	}

	// Setup IPC command handlers
	app.setupHandlers()

	// Start IPC protocol
	app.protocol.Start()

	// Send ready status
	app.protocol.SendStatus(ipc.StatusEventData{
		State: string(tsnet.StateStopped),
	})

	// Wait for shutdown signal
	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)
	<-sigCh

	app.shutdown()
}

func (a *App) setupHandlers() {
	a.protocol.OnCommand(ipc.CmdStart, a.handleStart)
	a.protocol.OnCommand(ipc.CmdStop, a.handleStop)
	a.protocol.OnCommand(ipc.CmdStatus, a.handleStatus)
	a.protocol.OnCommand(ipc.CmdWsMessage, a.handleWsMessage)
	a.protocol.OnCommand(ipc.CmdGetPeers, a.handleGetPeers)
	a.protocol.OnCommand(ipc.CmdDial, a.handleDial)
	a.protocol.OnCommand(ipc.CmdDialClose, a.handleDialClose)
	a.protocol.OnCommand(ipc.CmdDialMessage, a.handleDialMessage)

	// Reverse proxy handlers
	a.protocol.OnCommand(ipc.CmdProxyAdd, a.handleProxyAdd)
	a.protocol.OnCommand(ipc.CmdProxyRemove, a.handleProxyRemove)
	a.protocol.OnCommand(ipc.CmdProxyList, a.handleProxyList)
}

func (a *App) handleStart(cmd ipc.Command) error {
	var params ipc.StartCommand
	if err := json.Unmarshal(cmd.Data, &params); err != nil {
		return err
	}

	// Create tsnet node (lock only for assignment)
	a.mu.Lock()
	a.node = tsnet.NewNode(tsnet.Config{
		Hostname: params.Hostname,
		StateDir: params.StateDir,
		AuthKey:  params.AuthKey,
	})
	node := a.node
	a.mu.Unlock()

	// Setup callbacks (must be done without holding lock since callbacks acquire it)
	node.OnAuth(func(authURL string) {
		a.protocol.SendAuthRequired(authURL)
	})

	node.OnStatus(func(state tsnet.State, hostname, ip string, err error) {
		a.mu.Lock()
		a.hostname = hostname
		a.ip = ip

		errStr := ""
		if err != nil {
			errStr = err.Error()
		}

		// Get and store full DNS name for HTTPS certificate validation
		if state == tsnet.StateRunning {
			if dn, dnErr := node.GetDNSName(a.ctx); dnErr == nil {
				a.dnsName = dn
			}
		}
		dnsName := a.dnsName
		a.mu.Unlock()

		a.protocol.SendStatus(ipc.StatusEventData{
			State:       string(state),
			Hostname:    hostname,
			DNSName:     dnsName,
			TailscaleIP: ip,
			Error:       errStr,
		})
	})

	// Start the node (may trigger OnStatus callback)
	if err := node.Start(a.ctx); err != nil {
		return err
	}

	// Create HTTP server
	a.server = server.NewServer(server.Config{
		Protocol:    a.protocol,
		GetHostname: func() string { return a.hostname },
		GetIP:       func() string { return a.ip },
		GetVAPIDKey: func() string { return a.vapidKey },
		PWAPath:     params.PWAPath,
	})

	// Create dialer for outgoing connections
	a.dialer = server.NewDialer(a.protocol, a.node.Dial)

	// Create proxy manager for reverse proxies
	a.proxyManager = server.NewProxyManager(
		a.protocol,
		a.node.ListenTLS,
		func() string {
			a.mu.RLock()
			defer a.mu.RUnlock()
			return a.dnsName
		},
	)

	// Start listening on tailnet with TLS (port 443)
	// Both PWA and desktop-to-desktop connections use TLS
	// - PWA browsers require HTTPS
	// - Desktop clients use TLS over Tailscale (tsnet.Dial + tls.Client)
	go func() {
		ln, err := a.node.ListenTLS("tcp", ":443")
		if err != nil {
			log.Printf("Failed to listen with TLS on :443: %v", err)
			a.protocol.SendError("LISTEN_ERROR", err.Error())
			return
		}
		log.Printf("Listening on tailnet at https://%s:443 (TLS)", a.hostname)
		if err := a.server.Serve(ln); err != nil {
			log.Printf("TLS server error: %v", err)
		}
	}()

	return nil
}

func (a *App) handleStop(cmd ipc.Command) error {
	return a.shutdown()
}

func (a *App) handleStatus(cmd ipc.Command) error {
	a.mu.RLock()
	defer a.mu.RUnlock()

	state := tsnet.StateStopped
	if a.node != nil {
		state = a.node.State()
	}

	return a.protocol.SendStatus(ipc.StatusEventData{
		State:       string(state),
		Hostname:    a.hostname,
		DNSName:     a.dnsName,
		TailscaleIP: a.ip,
	})
}

func (a *App) handleWsMessage(cmd ipc.Command) error {
	var params ipc.WsMessageCommand
	if err := json.Unmarshal(cmd.Data, &params); err != nil {
		return err
	}

	a.mu.RLock()
	server := a.server
	a.mu.RUnlock()

	if server == nil {
		return nil
	}

	return server.SendToConnection(params.ConnectionID, []byte(params.Data))
}

func (a *App) handleGetPeers(cmd ipc.Command) error {
	a.mu.RLock()
	node := a.node
	a.mu.RUnlock()

	if node == nil {
		return a.protocol.SendPeers([]ipc.TailnetPeer{})
	}

	// Only show peers with "ccotg-" prefix (our app devices: desktop and mobile)
	peers, err := node.GetPeersFiltered(a.ctx, "ccotg-")
	if err != nil {
		log.Printf("Failed to get peers: %v", err)
		return a.protocol.SendPeers([]ipc.TailnetPeer{})
	}

	ipcPeers := make([]ipc.TailnetPeer, len(peers))
	for i, p := range peers {
		ipcPeers[i] = ipc.TailnetPeer{
			ID:           p.ID,
			Hostname:     p.Hostname,
			DNSName:      p.DNSName,
			TailscaleIPs: p.TailscaleIPs,
			Online:       p.Online,
			OS:           p.OS,
		}
	}

	return a.protocol.SendPeers(ipcPeers)
}

func (a *App) handleDial(cmd ipc.Command) error {
	var params ipc.DialCommand
	if err := json.Unmarshal(cmd.Data, &params); err != nil {
		return err
	}

	a.mu.RLock()
	dialer := a.dialer
	a.mu.RUnlock()

	if dialer == nil {
		return a.protocol.SendDialError(params.DeviceID, "node not started")
	}

	// Dial in a goroutine to not block IPC
	go func() {
		port := params.Port
		if port == 0 {
			port = 443
		}
		if err := dialer.Dial(a.ctx, params.DeviceID, params.Hostname, params.DNSName, port); err != nil {
			log.Printf("Dial failed: %v", err)
			// Error already sent via SendDialError in Dial()
		}
	}()

	return nil
}

func (a *App) handleDialClose(cmd ipc.Command) error {
	var params ipc.DialCloseCommand
	if err := json.Unmarshal(cmd.Data, &params); err != nil {
		return err
	}

	a.mu.RLock()
	dialer := a.dialer
	a.mu.RUnlock()

	if dialer != nil {
		dialer.Close(params.DeviceID)
	}

	return nil
}

func (a *App) handleDialMessage(cmd ipc.Command) error {
	var params ipc.DialMessageCommand
	if err := json.Unmarshal(cmd.Data, &params); err != nil {
		return err
	}

	a.mu.RLock()
	dialer := a.dialer
	a.mu.RUnlock()

	if dialer == nil {
		return nil
	}

	return dialer.Send(params.DeviceID, []byte(params.Data))
}

func (a *App) handleProxyAdd(cmd ipc.Command) error {
	var params ipc.ProxyAddCommand
	if err := json.Unmarshal(cmd.Data, &params); err != nil {
		return err
	}

	a.mu.RLock()
	proxyManager := a.proxyManager
	a.mu.RUnlock()

	if proxyManager == nil {
		return a.protocol.SendProxyError(params.ID, "node not started", "NOT_STARTED")
	}

	// Add proxy in a goroutine to not block IPC
	go func() {
		if err := proxyManager.Add(params.ID, params.Name, params.Port, params.TargetPort, params.TargetScheme); err != nil {
			log.Printf("Failed to add proxy: %v", err)
			// Error already sent via SendProxyError in Add()
		}
	}()

	return nil
}

func (a *App) handleProxyRemove(cmd ipc.Command) error {
	var params ipc.ProxyRemoveCommand
	if err := json.Unmarshal(cmd.Data, &params); err != nil {
		return err
	}

	a.mu.RLock()
	proxyManager := a.proxyManager
	a.mu.RUnlock()

	if proxyManager == nil {
		return nil
	}

	if err := proxyManager.Remove(params.ID); err != nil {
		log.Printf("Failed to remove proxy: %v", err)
	}

	return nil
}

func (a *App) handleProxyList(cmd ipc.Command) error {
	a.mu.RLock()
	proxyManager := a.proxyManager
	a.mu.RUnlock()

	if proxyManager == nil {
		return a.protocol.SendProxyList([]ipc.ProxyConfig{})
	}

	proxies := proxyManager.List()
	return a.protocol.SendProxyList(proxies)
}

func (a *App) shutdown() error {
	a.mu.Lock()
	defer a.mu.Unlock()

	// Close all reverse proxies
	if a.proxyManager != nil {
		a.proxyManager.CloseAll()
		a.proxyManager = nil
	}

	// Close all outgoing connections
	if a.dialer != nil {
		a.dialer.CloseAll()
		a.dialer = nil
	}

	if a.server != nil {
		a.server.Shutdown(a.ctx)
		a.server = nil
	}

	if a.node != nil {
		a.node.Stop()
		a.node = nil
	}

	a.protocol.Stop()
	a.cancel()

	return nil
}
