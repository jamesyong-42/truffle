// Package server provides HTTP handlers for the mesh network.
package server

import (
	"encoding/json"
	"net/http"
)

// HealthResponse represents the health check response
type HealthResponse struct {
	Status   string `json:"status"`
	Hostname string `json:"hostname,omitempty"`
	IP       string `json:"ip,omitempty"`
}

// StatusHandler returns the current status of the node
type StatusHandler struct {
	getStatus func() HealthResponse
}

// NewStatusHandler creates a new status handler
func NewStatusHandler(getStatus func() HealthResponse) *StatusHandler {
	return &StatusHandler{getStatus: getStatus}
}

func (h *StatusHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	status := h.getStatus()
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(status)
}

// CORSMiddleware adds CORS headers for PWA access
func CORSMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// Allow requests from PWA
		w.Header().Set("Access-Control-Allow-Origin", "*")
		w.Header().Set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
		w.Header().Set("Access-Control-Allow-Headers", "Content-Type, Authorization")

		if r.Method == "OPTIONS" {
			w.WriteHeader(http.StatusOK)
			return
		}

		next.ServeHTTP(w, r)
	})
}

// VAPIDPublicKeyHandler returns the VAPID public key for Web Push
type VAPIDPublicKeyHandler struct {
	getPublicKey func() string
}

// NewVAPIDPublicKeyHandler creates a handler for VAPID public key
func NewVAPIDPublicKeyHandler(getPublicKey func() string) *VAPIDPublicKeyHandler {
	return &VAPIDPublicKeyHandler{getPublicKey: getPublicKey}
}

func (h *VAPIDPublicKeyHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	publicKey := h.getPublicKey()
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]string{
		"publicKey": publicKey,
	})
}
