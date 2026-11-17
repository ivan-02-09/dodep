package middleware

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/pem"
	"os"
	"path/filepath"
	"testing"
)

// The middleware used to read only a PEM file, so a deployment that set the
// documented base64 variable silently fell back to HMAC. These cases pin the
// accepted encodings.
func TestLoadEd25519PublicKeyAcceptsAllDocumentedForms(t *testing.T) {
	pub, priv, _ := ed25519.GenerateKey(rand.Reader)
	b64pub := base64.StdEncoding.EncodeToString(pub)
	if !parseEd25519PublicKeyBase64(b64pub).Equal(pub) {
		t.Fatal("raw base64")
	}
	if !parseEd25519PublicKeyPEM(pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: pub})).Equal(pub) {
		t.Fatal("pem raw")
	}
	if !parseEd25519PublicKeyBase64(base64.StdEncoding.EncodeToString(priv)).Equal(pub) {
		t.Fatal("private base64")
	}
	dir := t.TempDir()
	path := filepath.Join(dir, "k.pem")
	os.WriteFile(path, pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: pub}), 0600)
	if !loadEd25519PublicKey(path).Equal(pub) {
		t.Fatal("file")
	}
	if parseEd25519PublicKeyBase64("not base64!!") != nil {
		t.Fatal("garbage should be nil")
	}
}
