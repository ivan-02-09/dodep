// Command kyc is the KYC (identity verification) service.
//
// It is currently a skeleton: the Sumsub integration and the gRPC API are not
// implemented yet. What it does provide is a real HTTP process, because the
// previous `func main() { // TODO }` exited instantly. That made the container
// restart in a loop, and since infra/docker/nginx/nginx.prod.conf declares
//
//	upstream kyc_upstream { server kyc:8089; }
//
// and nginx resolves upstream hostnames at start-up, an absent kyc container
// also kept the whole gateway from booting ("host not found in upstream kyc").
//
// Every unimplemented endpoint answers 501 with an explicit message instead of
// pretending to succeed: a caller must never mistake a stub for a verdict on a
// player's identity.
package main

import (
	"encoding/json"
	"log"
	"net/http"
	"os"
	"time"
)

func main() {
	port := os.Getenv("PORT")
	if port == "" {
		port = "8089"
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/health", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, http.StatusOK, map[string]string{"status": "ok", "service": "kyc"})
	})
	mux.HandleFunc("/ready", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, http.StatusOK, map[string]string{"status": "ready", "service": "kyc"})
	})
	mux.HandleFunc("/", notImplemented)

	srv := &http.Server{
		Addr:              ":" + port,
		Handler:           mux,
		ReadHeaderTimeout: 5 * time.Second,
	}

	// A gRPC listener is deliberately absent: kyc has no generated service and
	// no client depends on it today (admin-bff and user reach Sumsub directly).
	// Claiming GRPC_PORT without serving it would only produce probes that
	// connect and hang, so it is reported rather than bound.
	if grpcPort := os.Getenv("GRPC_PORT"); grpcPort != "" {
		log.Printf("kyc: gRPC API is not implemented; GRPC_PORT=%s is ignored", grpcPort)
	}

	log.Printf("kyc: HTTP listening on :%s (skeleton, KYC endpoints answer 501)", port)
	if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
		log.Fatalf("kyc: http server: %v", err)
	}
}

func notImplemented(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusNotImplemented, map[string]string{
		"error": "kyc service is not implemented yet",
		"path":  r.URL.Path,
	})
}

func writeJSON(w http.ResponseWriter, status int, body any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	if err := json.NewEncoder(w).Encode(body); err != nil {
		log.Printf("kyc: encode response: %v", err)
	}
}
