// Package server provides the HTTP/WebSocket server for the tsnet sidecar.
package server

import (
	"context"
	"net"
	"net/http"
	"sync"

	"github.com/jamesyong-42/claude-code-on-the-go/tsnet-sidecar/internal/ipc"
)

// Server wraps the HTTP server with WebSocket support
type Server struct {
	mux         *http.ServeMux
	httpServer  *http.Server
	connManager *ConnectionManager
	listener    net.Listener
	protocol    *ipc.Protocol
	mu          sync.RWMutex

	// Status callbacks
	getHostname func() string
	getIP       func() string
	getVAPIDKey func() string

	// PWA serving
	pwaPath string
}

// Config holds server configuration
type Config struct {
	Protocol    *ipc.Protocol
	GetHostname func() string
	GetIP       func() string
	GetVAPIDKey func() string
	PWAPath     string // Path to PWA dist directory (optional)
}

// NewServer creates a new HTTP server
func NewServer(cfg Config) *Server {
	s := &Server{
		mux:         http.NewServeMux(),
		protocol:    cfg.Protocol,
		connManager: NewConnectionManager(cfg.Protocol),
		getHostname: cfg.GetHostname,
		getIP:       cfg.GetIP,
		getVAPIDKey: cfg.GetVAPIDKey,
		pwaPath:     cfg.PWAPath,
	}

	s.setupRoutes()
	return s
}

func (s *Server) setupRoutes() {
	// Health/status endpoint
	s.mux.Handle("/health", CORSMiddleware(NewStatusHandler(s.healthStatus)))
	s.mux.Handle("/status", CORSMiddleware(NewStatusHandler(s.healthStatus)))

	// VAPID public key for Web Push
	if s.getVAPIDKey != nil {
		s.mux.Handle("/vapid", CORSMiddleware(NewVAPIDPublicKeyHandler(s.getVAPIDKey)))
	}

	// WebSocket endpoint
	s.mux.HandleFunc("/ws", func(w http.ResponseWriter, r *http.Request) {
		s.connManager.HandleWebSocket(w, r)
	})

	// Serve PWA static files or fallback landing page
	if s.pwaPath != "" {
		// Serve PWA with SPA fallback (index.html for unknown routes)
		fs := http.FileServer(http.Dir(s.pwaPath))
		s.mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
			// Try to serve the file
			path := r.URL.Path
			if path == "/" {
				path = "/index.html"
			}

			// Check if file exists
			fullPath := s.pwaPath + path
			if _, err := http.Dir(s.pwaPath).Open(path); err == nil {
				fs.ServeHTTP(w, r)
				return
			}

			// SPA fallback: serve index.html for client-side routing
			if _, err := http.Dir(s.pwaPath).Open("/index.html"); err == nil {
				r.URL.Path = "/"
				fs.ServeHTTP(w, r)
				return
			}

			// No PWA files found
			http.NotFound(w, r)
			_ = fullPath // suppress unused warning
		})
	} else {
		// Fallback landing page when no PWA path configured
		s.mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
			if r.URL.Path != "/" {
				http.NotFound(w, r)
				return
			}
			w.Header().Set("Content-Type", "text/html; charset=utf-8")
			hostname := ""
			if s.getHostname != nil {
				hostname = s.getHostname()
			}
			html := `<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Claude Code on the Go</title>
    <style>
        body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
               background: #1a1b26; color: #c0caf5; padding: 40px; max-width: 600px; margin: 0 auto; }
        h1 { color: #7aa2f7; }
        .status { background: #24283b; padding: 20px; border-radius: 8px; margin: 20px 0; }
        .ok { color: #9ece6a; }
        code { background: #414868; padding: 2px 6px; border-radius: 4px; }
        a { color: #7aa2f7; }
    </style>
</head>
<body>
    <h1>🚀 Claude Code on the Go</h1>
    <div class="status">
        <p><strong>Status:</strong> <span class="ok">Running</span></p>
        <p><strong>Hostname:</strong> <code>` + hostname + `</code></p>
        <p><strong>WebSocket:</strong> <code>wss://` + r.Host + `/ws</code></p>
    </div>
    <p>This is the mesh networking endpoint for Claude Code on the Go.</p>
    <p>Connect your PWA or mobile app using the WebSocket URL above.</p>
    <p><em>Note: PWA not configured. Set pwaPath in start command to serve the PWA.</em></p>
</body>
</html>`
			w.Write([]byte(html))
		})
	}
}

func (s *Server) healthStatus() HealthResponse {
	resp := HealthResponse{
		Status: "ok",
	}

	if s.getHostname != nil {
		resp.Hostname = s.getHostname()
	}

	if s.getIP != nil {
		resp.IP = s.getIP()
	}

	return resp
}

// Handler returns the HTTP handler
func (s *Server) Handler() http.Handler {
	return s.mux
}

// Serve starts the HTTP server on the provided listener
func (s *Server) Serve(ln net.Listener) error {
	s.mu.Lock()
	s.listener = ln
	s.httpServer = &http.Server{Handler: s.mux}
	s.mu.Unlock()

	return s.httpServer.Serve(ln)
}

// Shutdown gracefully shuts down the server
func (s *Server) Shutdown(ctx context.Context) error {
	s.mu.Lock()
	server := s.httpServer
	s.mu.Unlock()

	// Close all WebSocket connections
	s.connManager.CloseAll()

	if server != nil {
		return server.Shutdown(ctx)
	}
	return nil
}

// ConnectionManager returns the WebSocket connection manager
func (s *Server) ConnectionManager() *ConnectionManager {
	return s.connManager
}

// SendToConnection sends data to a specific WebSocket connection
func (s *Server) SendToConnection(connID string, data []byte) error {
	return s.connManager.Send(connID, data)
}

// Broadcast sends data to all WebSocket connections
func (s *Server) Broadcast(data []byte) {
	s.connManager.Broadcast(data)
}
