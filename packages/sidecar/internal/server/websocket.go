// Package server provides HTTP/WebSocket handling for the tsnet sidecar.
package server

import (
	"encoding/json"
	"log"
	"net/http"
	"sync"

	"github.com/gorilla/websocket"
	"github.com/jamesyong-42/claude-code-on-the-go/tsnet-sidecar/internal/ipc"
)

var upgrader = websocket.Upgrader{
	ReadBufferSize:  1024,
	WriteBufferSize: 1024,
	CheckOrigin: func(r *http.Request) bool {
		// Allow all origins for now - PWA will connect from various origins
		return true
	},
}

// Connection represents a WebSocket connection
type Connection struct {
	ID       string
	Conn     *websocket.Conn
	Send     chan []byte
	Done     chan struct{}
	IsPWA    bool
	PeerID   string
	mu       sync.Mutex
}

// ConnectionManager manages WebSocket connections
type ConnectionManager struct {
	connections map[string]*Connection
	protocol    *ipc.Protocol
	mu          sync.RWMutex
	onMessage   func(connID string, data []byte)
	nextID      int
}

// NewConnectionManager creates a new connection manager
func NewConnectionManager(protocol *ipc.Protocol) *ConnectionManager {
	return &ConnectionManager{
		connections: make(map[string]*Connection),
		protocol:    protocol,
	}
}

// OnMessage sets the callback for incoming WebSocket messages
func (cm *ConnectionManager) OnMessage(cb func(connID string, data []byte)) {
	cm.onMessage = cb
}

// HandleWebSocket upgrades HTTP to WebSocket and manages the connection
func (cm *ConnectionManager) HandleWebSocket(w http.ResponseWriter, r *http.Request) {
	conn, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		log.Printf("WebSocket upgrade failed: %v", err)
		return
	}

	cm.mu.Lock()
	cm.nextID++
	connID := json.Number(json.Number(string(rune('0' + cm.nextID)))).String()
	connID = r.RemoteAddr // Use remote addr as ID for now
	cm.mu.Unlock()

	c := &Connection{
		ID:   connID,
		Conn: conn,
		Send: make(chan []byte, 256),
		Done: make(chan struct{}),
	}

	cm.mu.Lock()
	cm.connections[connID] = c
	cm.mu.Unlock()

	// Notify Electron of new connection
	if cm.protocol != nil {
		cm.protocol.SendWsConnect(connID, r.RemoteAddr)
	}

	// Start read/write goroutines
	go cm.readPump(c)
	go cm.writePump(c)
}

// Send sends data to a specific connection
func (cm *ConnectionManager) Send(connID string, data []byte) error {
	cm.mu.RLock()
	conn, ok := cm.connections[connID]
	cm.mu.RUnlock()

	if !ok {
		return nil // Connection doesn't exist
	}

	select {
	case conn.Send <- data:
	default:
		// Buffer full, close connection
		cm.closeConnection(connID, "buffer full")
	}

	return nil
}

// Broadcast sends data to all connections
func (cm *ConnectionManager) Broadcast(data []byte) {
	cm.mu.RLock()
	defer cm.mu.RUnlock()

	for _, conn := range cm.connections {
		select {
		case conn.Send <- data:
		default:
			// Skip if buffer full
		}
	}
}

// Close closes a specific connection
func (cm *ConnectionManager) Close(connID string) {
	cm.closeConnection(connID, "closed by server")
}

// CloseAll closes all connections
func (cm *ConnectionManager) CloseAll() {
	cm.mu.Lock()
	connIDs := make([]string, 0, len(cm.connections))
	for id := range cm.connections {
		connIDs = append(connIDs, id)
	}
	cm.mu.Unlock()

	for _, id := range connIDs {
		cm.closeConnection(id, "server shutdown")
	}
}

func (cm *ConnectionManager) closeConnection(connID, reason string) {
	cm.mu.Lock()
	conn, ok := cm.connections[connID]
	if ok {
		delete(cm.connections, connID)
	}
	cm.mu.Unlock()

	if !ok {
		return
	}

	close(conn.Done)
	conn.Conn.Close()

	// Notify Electron
	if cm.protocol != nil {
		cm.protocol.SendWsDisconnect(connID, reason)
	}
}

func (cm *ConnectionManager) readPump(c *Connection) {
	defer func() {
		cm.closeConnection(c.ID, "read pump closed")
	}()

	for {
		_, message, err := c.Conn.ReadMessage()
		if err != nil {
			if websocket.IsUnexpectedCloseError(err, websocket.CloseGoingAway, websocket.CloseAbnormalClosure) {
				log.Printf("WebSocket read error: %v", err)
			}
			return
		}

		// Forward to Electron via IPC
		if cm.protocol != nil {
			cm.protocol.SendWsMessage(c.ID, string(message))
		}

		// Call message handler
		if cm.onMessage != nil {
			cm.onMessage(c.ID, message)
		}
	}
}

func (cm *ConnectionManager) writePump(c *Connection) {
	defer func() {
		c.Conn.Close()
	}()

	for {
		select {
		case message, ok := <-c.Send:
			if !ok {
				c.Conn.WriteMessage(websocket.CloseMessage, []byte{})
				return
			}

			c.mu.Lock()
			err := c.Conn.WriteMessage(websocket.TextMessage, message)
			c.mu.Unlock()

			if err != nil {
				log.Printf("WebSocket write error: %v", err)
				return
			}

		case <-c.Done:
			return
		}
	}
}
