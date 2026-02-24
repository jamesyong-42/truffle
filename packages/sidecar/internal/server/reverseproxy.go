// Package server provides the HTTP/WebSocket server for the tsnet sidecar.
package server

import (
	"context"
	"crypto/tls"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/jamesyong-42/claude-code-on-the-go/tsnet-sidecar/internal/ipc"
)

// ListenTLSFunc is a function that creates a TLS listener on the given address
type ListenTLSFunc func(network, addr string) (net.Listener, error)

// GetDNSNameFunc returns the full MagicDNS name for URL generation
type GetDNSNameFunc func() string

// ReverseProxy represents a single reverse proxy instance
type ReverseProxy struct {
	ID           string
	Name         string
	Port         int
	TargetPort   int
	TargetScheme string
	listener     net.Listener
	server       *http.Server
	cancel       context.CancelFunc
}

// ProxyManager manages multiple reverse proxy instances
type ProxyManager struct {
	proxies      map[string]*ReverseProxy
	mu           sync.RWMutex
	protocol     *ipc.Protocol
	listenTLS    ListenTLSFunc
	getDNSName   GetDNSNameFunc
}

// NewProxyManager creates a new ProxyManager
func NewProxyManager(protocol *ipc.Protocol, listenTLS ListenTLSFunc, getDNSName GetDNSNameFunc) *ProxyManager {
	return &ProxyManager{
		proxies:    make(map[string]*ReverseProxy),
		protocol:   protocol,
		listenTLS:  listenTLS,
		getDNSName: getDNSName,
	}
}

// Add creates and starts a new reverse proxy
func (pm *ProxyManager) Add(id, name string, port, targetPort int, targetScheme string) error {
	pm.mu.Lock()

	// Default to http if not specified
	if targetScheme == "" {
		targetScheme = "http"
	}

	// Check if proxy with this ID already exists
	if _, exists := pm.proxies[id]; exists {
		pm.mu.Unlock()
		errMsg := fmt.Sprintf("proxy with id %s already exists", id)
		pm.protocol.SendProxyError(id, errMsg, "PROXY_EXISTS")
		return fmt.Errorf("proxy with id %s already exists", id)
	}

	// Check if port is already in use by another proxy
	for _, proxy := range pm.proxies {
		if proxy.Port == port {
			pm.mu.Unlock()
			errMsg := fmt.Sprintf("port %d is already in use by proxy %s", port, proxy.ID)
			pm.protocol.SendProxyError(id, errMsg, "PORT_IN_USE")
			return fmt.Errorf("port %d is already in use by proxy %s", port, proxy.ID)
		}
	}
	pm.mu.Unlock()

	// Create TLS listener on the specified port
	addr := fmt.Sprintf(":%d", port)
	log.Printf("[ProxyManager] Proxy %s: calling ListenTLS on %s", id, addr)
	ln, err := pm.listenTLS("tcp", addr)
	if err != nil {
		errMsg := fmt.Sprintf("failed to listen on port %d: %v", port, err)
		code := "LISTEN_ERROR"
		if strings.Contains(err.Error(), "address already in use") {
			code = "PORT_IN_USE"
		}
		pm.protocol.SendProxyError(id, errMsg, code)
		return fmt.Errorf("failed to listen on port %d: %w", port, err)
	}
	log.Printf("[ProxyManager] Proxy %s: ListenTLS returned listener type %T, addr %s", id, ln, ln.Addr().String())

	// Create reverse proxy handler
	targetURL := &url.URL{
		Scheme: targetScheme,
		Host:   fmt.Sprintf("localhost:%d", targetPort),
	}
	reverseProxy := httputil.NewSingleHostReverseProxy(targetURL)

	// Rewrite Host header to localhost so dev servers (Vite, etc.) don't reject the request
	originalDirector := reverseProxy.Director
	reverseProxy.Director = func(req *http.Request) {
		originalDirector(req)
		req.Host = fmt.Sprintf("localhost:%d", targetPort)
	}

	// Custom error handler
	reverseProxy.ErrorHandler = func(w http.ResponseWriter, r *http.Request, err error) {
		log.Printf("[ProxyManager] Proxy %s error: %v", id, err)

		// Check for connection refused (target not running)
		if strings.Contains(err.Error(), "connection refused") {
			pm.protocol.SendProxyError(id, fmt.Sprintf("target localhost:%d not reachable", targetPort), "CONNECTION_REFUSED")
		}

		http.Error(w, "Bad Gateway", http.StatusBadGateway)
	}

	// Custom transport with timeout
	// For HTTPS targets, skip certificate verification (common for local dev servers with self-signed certs)
	transport := &http.Transport{
		DialContext: (&net.Dialer{
			Timeout:   5 * time.Second,
			KeepAlive: 30 * time.Second,
		}).DialContext,
		MaxIdleConns:          100,
		IdleConnTimeout:       90 * time.Second,
		TLSHandshakeTimeout:   10 * time.Second,
		ExpectContinueTimeout: 1 * time.Second,
	}
	if targetScheme == "https" {
		transport.TLSClientConfig = &tls.Config{
			InsecureSkipVerify: true, // Allow self-signed certificates for local dev servers
		}
	}
	reverseProxy.Transport = transport

	// Create HTTP handler that supports both regular HTTP and WebSocket
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		log.Printf("[ProxyManager] Proxy %s received request: %s %s (Host: %s)", id, r.Method, r.URL.Path, r.Host)
		// Check if this is a WebSocket upgrade request (for HMR)
		if isWebSocketRequest(r) {
			log.Printf("[ProxyManager] Proxy %s: WebSocket upgrade detected, proxying to %s://localhost:%d", id, targetScheme, targetPort)
			pm.handleWebSocketProxy(w, r, targetPort, targetScheme)
			return
		}
		reverseProxy.ServeHTTP(w, r)
	})

	// Create context for graceful shutdown
	ctx, cancel := context.WithCancel(context.Background())

	// Create HTTP server
	server := &http.Server{
		Handler: handler,
	}

	// Create and store the proxy
	proxy := &ReverseProxy{
		ID:           id,
		Name:         name,
		Port:         port,
		TargetPort:   targetPort,
		TargetScheme: targetScheme,
		listener:     ln,
		server:       server,
		cancel:       cancel,
	}

	pm.mu.Lock()
	pm.proxies[id] = proxy
	pm.mu.Unlock()

	// Wrap listener with logging to debug connection issues
	wrappedLn := &loggingListener{Listener: ln, id: id}
	log.Printf("[ProxyManager] Proxy %s: wrapped listener created, starting server.Serve()", id)

	// Start serving in a goroutine
	go func() {
		log.Printf("[ProxyManager] Starting proxy %s: port %d -> localhost:%d (goroutine started)", id, port, targetPort)
		if err := server.Serve(wrappedLn); err != nil && err != http.ErrServerClosed {
			log.Printf("[ProxyManager] Proxy %s serve error: %v", id, err)
			pm.protocol.SendProxyError(id, err.Error(), "SERVE_ERROR")
		}
		log.Printf("[ProxyManager] Proxy %s: server.Serve() returned", id)
	}()

	// Wait for context cancellation to trigger shutdown
	go func() {
		<-ctx.Done()
		shutdownCtx, shutdownCancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer shutdownCancel()
		server.Shutdown(shutdownCtx)
	}()

	// Generate URL and send success event
	dnsName := ""
	if pm.getDNSName != nil {
		dnsName = pm.getDNSName()
	}
	proxyURL := fmt.Sprintf("https://%s:%d", dnsName, port)
	log.Printf("[ProxyManager] Proxy %s started successfully at %s", id, proxyURL)
	pm.protocol.SendProxyStarted(id, port, targetPort, proxyURL)

	return nil
}

// Remove stops and removes a reverse proxy
func (pm *ProxyManager) Remove(id string) error {
	pm.mu.Lock()
	proxy, exists := pm.proxies[id]
	if !exists {
		pm.mu.Unlock()
		return fmt.Errorf("proxy %s not found", id)
	}
	delete(pm.proxies, id)
	pm.mu.Unlock()

	// Trigger graceful shutdown
	if proxy.cancel != nil {
		proxy.cancel()
	}

	// Close listener
	if proxy.listener != nil {
		proxy.listener.Close()
	}

	log.Printf("[ProxyManager] Proxy %s removed", id)
	pm.protocol.SendProxyStopped(id, "removed")
	return nil
}

// List returns all proxy configurations
func (pm *ProxyManager) List() []ipc.ProxyConfig {
	pm.mu.RLock()
	defer pm.mu.RUnlock()

	configs := make([]ipc.ProxyConfig, 0, len(pm.proxies))
	for _, proxy := range pm.proxies {
		configs = append(configs, ipc.ProxyConfig{
			ID:           proxy.ID,
			Name:         proxy.Name,
			Port:         proxy.Port,
			TargetPort:   proxy.TargetPort,
			TargetScheme: proxy.TargetScheme,
			IsActive:     true, // If it's in the map, it's active
		})
	}
	return configs
}

// CloseAll stops and removes all proxies
func (pm *ProxyManager) CloseAll() {
	pm.mu.Lock()
	proxies := make([]*ReverseProxy, 0, len(pm.proxies))
	for _, proxy := range pm.proxies {
		proxies = append(proxies, proxy)
	}
	pm.proxies = make(map[string]*ReverseProxy)
	pm.mu.Unlock()

	for _, proxy := range proxies {
		if proxy.cancel != nil {
			proxy.cancel()
		}
		if proxy.listener != nil {
			proxy.listener.Close()
		}
		log.Printf("[ProxyManager] Proxy %s closed", proxy.ID)
		pm.protocol.SendProxyStopped(proxy.ID, "shutdown")
	}
}

// isWebSocketRequest checks if the request is a WebSocket upgrade request
func isWebSocketRequest(r *http.Request) bool {
	connection := strings.ToLower(r.Header.Get("Connection"))
	upgrade := strings.ToLower(r.Header.Get("Upgrade"))
	isWS := strings.Contains(connection, "upgrade") && upgrade == "websocket"
	if connection != "" || upgrade != "" {
		log.Printf("[ProxyManager] WebSocket check: Connection=%q, Upgrade=%q, isWebSocket=%v", connection, upgrade, isWS)
	}
	return isWS
}

// handleWebSocketProxy handles WebSocket connections by hijacking and proxying
func (pm *ProxyManager) handleWebSocketProxy(w http.ResponseWriter, r *http.Request, targetPort int, targetScheme string) {
	// Connect to the target WebSocket server
	targetAddr := fmt.Sprintf("localhost:%d", targetPort)
	var targetConn net.Conn
	var err error

	log.Printf("[ProxyManager] WebSocket: dialing %s://%s", targetScheme, targetAddr)

	if targetScheme == "https" {
		// For HTTPS targets, use TLS with InsecureSkipVerify for self-signed certs
		dialer := &net.Dialer{Timeout: 5 * time.Second}
		targetConn, err = tls.DialWithDialer(dialer, "tcp", targetAddr, &tls.Config{
			InsecureSkipVerify: true,
		})
	} else {
		targetConn, err = net.DialTimeout("tcp", targetAddr, 5*time.Second)
	}

	if err != nil {
		log.Printf("[ProxyManager] WebSocket: failed to connect to target %s (%s): %v", targetAddr, targetScheme, err)
		http.Error(w, "Bad Gateway", http.StatusBadGateway)
		return
	}
	log.Printf("[ProxyManager] WebSocket: connected to target %s", targetAddr)

	// Hijack the client connection
	hijacker, ok := w.(http.Hijacker)
	if !ok {
		log.Printf("[ProxyManager] WebSocket: response writer does not support hijacking")
		targetConn.Close()
		http.Error(w, "Internal Server Error", http.StatusInternalServerError)
		return
	}

	clientConn, _, err := hijacker.Hijack()
	if err != nil {
		log.Printf("[ProxyManager] WebSocket: failed to hijack connection: %v", err)
		targetConn.Close()
		http.Error(w, "Internal Server Error", http.StatusInternalServerError)
		return
	}
	log.Printf("[ProxyManager] WebSocket: hijacked client connection")

	// Rewrite Host header to match target (Vite rejects mismatched hosts)
	originalHost := r.Host
	r.Host = targetAddr
	r.Header.Set("Host", targetAddr)
	log.Printf("[ProxyManager] WebSocket: rewrote Host header from %s to %s", originalHost, targetAddr)

	// Forward the original request to the target
	log.Printf("[ProxyManager] WebSocket: forwarding request %s %s", r.Method, r.URL.String())
	if err := r.Write(targetConn); err != nil {
		log.Printf("[ProxyManager] WebSocket: failed to forward request: %v", err)
		clientConn.Close()
		targetConn.Close()
		return
	}
	log.Printf("[ProxyManager] WebSocket: request forwarded, starting bidirectional copy")

	// Bidirectional copy
	go func() {
		n, err := io.Copy(targetConn, clientConn)
		log.Printf("[ProxyManager] WebSocket: client->target copy ended: %d bytes, err=%v", n, err)
		targetConn.Close()
	}()
	go func() {
		n, err := io.Copy(clientConn, targetConn)
		log.Printf("[ProxyManager] WebSocket: target->client copy ended: %d bytes, err=%v", n, err)
		clientConn.Close()
	}()
}

// loggingListener wraps a net.Listener to log Accept calls for debugging
type loggingListener struct {
	net.Listener
	id string
}

func (l *loggingListener) Accept() (net.Conn, error) {
	log.Printf("[ProxyManager] Proxy %s: Accept() called, waiting for connection...", l.id)
	conn, err := l.Listener.Accept()
	if err != nil {
		log.Printf("[ProxyManager] Proxy %s: Accept() error: %v", l.id, err)
		return nil, err
	}
	log.Printf("[ProxyManager] Proxy %s: Accept() got connection from %s", l.id, conn.RemoteAddr())
	return conn, nil
}
