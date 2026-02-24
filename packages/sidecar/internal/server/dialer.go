// Package server provides WebSocket client (dialer) for outgoing connections.
package server

import (
	"context"
	"crypto/tls"
	"fmt"
	"log"
	"net"
	"net/http"
	"sync"
	"time"

	"github.com/gorilla/websocket"
	"github.com/jamesyong-42/claude-code-on-the-go/tsnet-sidecar/internal/ipc"
)

const (
	dialTimeout     = 10 * time.Second
	dialPingPeriod  = 30 * time.Second
	dialPongWait    = 35 * time.Second
	dialWriteWait   = 10 * time.Second
	dialBufferSize  = 256
)

// DialConnection represents an outgoing WebSocket connection
type DialConnection struct {
	DeviceID   string
	Hostname   string
	Port       int
	conn       *websocket.Conn
	sendCh     chan []byte
	closeCh    chan struct{}
	closeOnce  sync.Once
}

// Dialer manages outgoing WebSocket connections over Tailscale
type Dialer struct {
	protocol    *ipc.Protocol
	dialFunc    func(ctx context.Context, network, addr string) (net.Conn, error)
	connections map[string]*DialConnection
	mu          sync.RWMutex
	onMessage   func(deviceID string, data []byte)
}

// NewDialer creates a new Dialer
// dialFunc should be tsnet.Server.Dial for Tailscale connections
func NewDialer(protocol *ipc.Protocol, dialFunc func(ctx context.Context, network, addr string) (net.Conn, error)) *Dialer {
	return &Dialer{
		protocol:    protocol,
		dialFunc:    dialFunc,
		connections: make(map[string]*DialConnection),
	}
}

// OnMessage sets the callback for incoming messages
func (d *Dialer) OnMessage(cb func(deviceID string, data []byte)) {
	d.onMessage = cb
}

// Dial establishes an outgoing WebSocket connection to a device
// Uses TLS over Tailscale for secure WebSocket (wss://) connections
func (d *Dialer) Dial(ctx context.Context, deviceID, hostname, dnsName string, port int) error {
	d.mu.Lock()
	if _, exists := d.connections[deviceID]; exists {
		d.mu.Unlock()
		return fmt.Errorf("connection to %s already exists", deviceID)
	}
	d.mu.Unlock()

	if port == 0 {
		port = 443
	}

	// Use DNS name for dial address if available (Tailscale routes by DNS name)
	dialHost := hostname
	if dnsName != "" {
		dialHost = dnsName
	}
	addr := fmt.Sprintf("%s:%d", dialHost, port)
	log.Printf("[Dialer] Connecting to %s (TLS over Tailscale)", addr)

	// Create a context with timeout
	dialCtx, cancel := context.WithTimeout(ctx, dialTimeout)
	defer cancel()

	// Determine the ServerName for TLS certificate validation
	// Use DNS name if available (matches the Let's Encrypt certificate)
	tlsServerName := dnsName
	if tlsServerName == "" {
		tlsServerName = hostname
	}

	// Upgrade to WebSocket over Tailscale with TLS
	// Server uses ListenTLS which provides Let's Encrypt certificates
	wsURL := fmt.Sprintf("wss://%s/ws", dialHost)
	dialer := websocket.Dialer{
		NetDialTLSContext: func(ctx context.Context, network, dialAddr string) (net.Conn, error) {
			// Dial using Tailscale's network
			netConn, err := d.dialFunc(ctx, network, addr)
			if err != nil {
				return nil, fmt.Errorf("tsnet dial failed: %w", err)
			}

			// Wrap with TLS (server uses ListenTLS with Let's Encrypt certs)
			tlsConfig := &tls.Config{
				ServerName: tlsServerName,
				// Use system root CAs (includes Let's Encrypt)
			}
			tlsConn := tls.Client(netConn, tlsConfig)

			// Perform TLS handshake with timeout
			if err := tlsConn.HandshakeContext(ctx); err != nil {
				netConn.Close()
				return nil, fmt.Errorf("TLS handshake failed: %w", err)
			}

			log.Printf("[Dialer] TLS handshake complete with %s (ServerName: %s)", addr, tlsServerName)
			return tlsConn, nil
		},
		HandshakeTimeout: dialTimeout,
	}

	wsConn, _, err := dialer.DialContext(dialCtx, wsURL, http.Header{})
	if err != nil {
		log.Printf("[Dialer] WebSocket connection failed for %s: %v", addr, err)
		d.protocol.SendDialError(deviceID, err.Error())
		return err
	}

	conn := &DialConnection{
		DeviceID: deviceID,
		Hostname: hostname,
		Port:     port,
		conn:     wsConn,
		sendCh:   make(chan []byte, dialBufferSize),
		closeCh:  make(chan struct{}),
	}

	d.mu.Lock()
	d.connections[deviceID] = conn
	d.mu.Unlock()

	log.Printf("[Dialer] Connected to %s (%s)", deviceID, addr)
	d.protocol.SendDialConnected(deviceID, addr)

	// Start read and write pumps
	go d.readPump(conn)
	go d.writePump(conn)

	return nil
}

// Send sends data to a specific outgoing connection
func (d *Dialer) Send(deviceID string, data []byte) error {
	d.mu.RLock()
	conn, ok := d.connections[deviceID]
	d.mu.RUnlock()

	if !ok {
		return fmt.Errorf("no connection to %s", deviceID)
	}

	select {
	case conn.sendCh <- data:
		return nil
	default:
		// Buffer full, close connection
		d.closeConnection(deviceID, "send buffer full")
		return fmt.Errorf("send buffer full")
	}
}

// Close closes a specific outgoing connection
func (d *Dialer) Close(deviceID string) {
	d.closeConnection(deviceID, "closed by client")
}

// CloseAll closes all outgoing connections
func (d *Dialer) CloseAll() {
	d.mu.Lock()
	deviceIDs := make([]string, 0, len(d.connections))
	for id := range d.connections {
		deviceIDs = append(deviceIDs, id)
	}
	d.mu.Unlock()

	for _, id := range deviceIDs {
		d.closeConnection(id, "shutdown")
	}
}

func (d *Dialer) closeConnection(deviceID, reason string) {
	d.mu.Lock()
	conn, ok := d.connections[deviceID]
	if ok {
		delete(d.connections, deviceID)
	}
	d.mu.Unlock()

	if !ok {
		return
	}

	conn.closeOnce.Do(func() {
		close(conn.closeCh)
		conn.conn.Close()
		log.Printf("[Dialer] Disconnected from %s: %s", deviceID, reason)
		d.protocol.SendDialDisconnect(deviceID, reason)
	})
}

func (d *Dialer) readPump(c *DialConnection) {
	defer func() {
		d.closeConnection(c.DeviceID, "read pump closed")
	}()

	c.conn.SetReadDeadline(time.Now().Add(dialPongWait))
	c.conn.SetPongHandler(func(string) error {
		c.conn.SetReadDeadline(time.Now().Add(dialPongWait))
		return nil
	})

	for {
		_, message, err := c.conn.ReadMessage()
		if err != nil {
			if websocket.IsUnexpectedCloseError(err, websocket.CloseGoingAway, websocket.CloseAbnormalClosure) {
				log.Printf("[Dialer] Read error from %s: %v", c.DeviceID, err)
			}
			return
		}

		// Notify Electron of incoming message
		if d.protocol != nil {
			d.protocol.SendDialMessage(c.DeviceID, string(message))
		}

		// Call message callback if set
		if d.onMessage != nil {
			d.onMessage(c.DeviceID, message)
		}
	}
}

func (d *Dialer) writePump(c *DialConnection) {
	ticker := time.NewTicker(dialPingPeriod)
	defer func() {
		ticker.Stop()
		d.closeConnection(c.DeviceID, "write pump closed")
	}()

	for {
		select {
		case <-c.closeCh:
			return
		case message, ok := <-c.sendCh:
			if !ok {
				return
			}
			c.conn.SetWriteDeadline(time.Now().Add(dialWriteWait))
			if err := c.conn.WriteMessage(websocket.TextMessage, message); err != nil {
				log.Printf("[Dialer] Write error to %s: %v", c.DeviceID, err)
				return
			}
		case <-ticker.C:
			c.conn.SetWriteDeadline(time.Now().Add(dialWriteWait))
			if err := c.conn.WriteMessage(websocket.PingMessage, nil); err != nil {
				return
			}
		}
	}
}
