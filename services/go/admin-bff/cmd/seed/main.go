package main

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"fmt"
	"log"
	"os"

	"github.com/jackc/pgx/v5/pgxpool"
	"golang.org/x/crypto/bcrypt"
)

// minSeedPasswordLength is the shortest password the seed will accept. The old
// default was "admin123" for a super_admin account: a published credential that
// anyone could use on any deployment that ran the seed and forgot to override
// ADMIN_PASSWORD.
const minSeedPasswordLength = 12

func main() {
	dsn := os.Getenv("DATABASE_URL")
	if dsn == "" {
		dsn = "postgres://postgres:changeme@localhost:5433/opus_casino?sslmode=disable"
	}
	email := os.Getenv("ADMIN_EMAIL")
	if email == "" {
		email = "admin@example.com"
	}
	password := os.Getenv("ADMIN_PASSWORD")
	if len(password) < minSeedPasswordLength {
		if password != "" {
			log.Fatalf("ADMIN_PASSWORD must be at least %d characters", minSeedPasswordLength)
		}
		// Generate instead of falling back to a known value. Printed once so an
		// operator can record it and change it at first login.
		generated := make([]byte, 24)
		if _, err := rand.Read(generated); err != nil {
			log.Fatalf("generate password: %v", err)
		}
		password = base64.RawURLEncoding.EncodeToString(generated)
		fmt.Printf("ADMIN_PASSWORD was not set; generated one-time password: %s\n", password)
		fmt.Println("Store it now and change it after the first login.")
	}
	if email == "admin@example.com" {
		log.Println("WARNING: ADMIN_EMAIL is the placeholder admin@example.com; set a real address")
	}

	db, err := pgxpool.New(context.Background(), dsn)
	if err != nil {
		log.Fatal(err)
	}
	defer db.Close()

	hash, err := bcrypt.GenerateFromPassword([]byte(password), bcrypt.DefaultCost)
	if err != nil {
		log.Fatal(err)
	}

	_, err = db.Exec(context.Background(), `
		INSERT INTO admin_users (email, password_hash, first_name, last_name, role, status, permissions, created_at, updated_at)
		VALUES ($1, $2, 'System', 'Admin', 'super_admin', 'active', ARRAY['*'], NOW(), NOW())
		ON CONFLICT (email) DO UPDATE SET
			password_hash = EXCLUDED.password_hash,
			status = 'active',
			updated_at = NOW()
	`, email, string(hash))
	if err != nil {
		log.Fatal(err)
	}

	fmt.Printf("Admin user created/updated: %s\n", email)
}
