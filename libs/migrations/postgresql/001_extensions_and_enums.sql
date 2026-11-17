-- Migration: 001_extensions_and_enums.sql
-- Description: Extensions, ENUM types, helper functions
-- Author: DATA_ENGINEER
-- Date: 2026-03-24

-- ============================================================
-- EXTENSIONS
-- ============================================================

-- pgcrypto is the only hard requirement (gen_random_uuid, digest, crypt).
CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- The remaining three are optional and environment dependent:
--   * citus            - only present in a Citus image; needed for sharding
--                        (see 008_rls_and_triggers.sql, which degrades
--                        gracefully when the extension is absent)
--   * pg_partman       - absent from most images; partitioning is a scale-out
--                        concern and no migration creates a partition yet
--   * pg_stat_statements - raises "must be loaded via shared_preload_libraries"
--                        on every stock image unless configured
--
-- A plain `CREATE EXTENSION` for any of these aborted the entire migration run
-- on a stock PostgreSQL, so they are attempted and skipped with a notice.
DO $$
DECLARE
    ext TEXT;
BEGIN
    FOREACH ext IN ARRAY ARRAY['citus', 'pg_partman', 'pg_stat_statements'] LOOP
        BEGIN
            EXECUTE format('CREATE EXTENSION IF NOT EXISTS %I', ext);
        EXCEPTION WHEN OTHERS THEN
            RAISE NOTICE 'optional extension % not available: %', ext, SQLERRM;
        END;
    END LOOP;
END
$$;

-- ============================================================
-- HELPER FUNCTION: auto-update updated_at
-- ============================================================

CREATE OR REPLACE FUNCTION update_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- ============================================================
-- ENUM TYPES
-- ============================================================

-- User statuses
CREATE TYPE user_status_enum AS ENUM (
    'pending',
    'active',
    'blocked',
    'self_excluded',
    'suspended',
    'closed'
);

-- KYC levels
CREATE TYPE kyc_level_enum AS ENUM (
    'none',
    'basic',
    'verified',
    'enhanced',
    'vip'
);

-- Transaction types
CREATE TYPE tx_type_enum AS ENUM (
    'deposit',
    'withdrawal',
    'bet_place',
    'bet_win',
    'bet_refund',
    'bonus_credit',
    'bonus_wager',
    'adjustment',
    'transfer_in',
    'transfer_out'
);

-- Transaction statuses
CREATE TYPE tx_status_enum AS ENUM (
    'pending',
    'processing',
    'completed',
    'failed',
    'reversed'
);

-- Bet types
CREATE TYPE bet_type_enum AS ENUM (
    'single',
    'accumulator',
    'system',
    'chain'
);

-- Bet statuses
CREATE TYPE bet_status_enum AS ENUM (
    'pending',
    'active',
    'won',
    'lost',
    'void',
    'cashout',
    'rejected'
);

-- Payment statuses
CREATE TYPE payment_status_enum AS ENUM (
    'initiated',
    'processing',
    'completed',
    'failed',
    'cancelled',
    'requires_review',
    'refunded'
);

-- Currency type
CREATE TYPE currency_type_enum AS ENUM (
    'fiat',
    'crypto'
);

-- Actor types for audit
CREATE TYPE actor_type_enum AS ENUM (
    'user',
    'admin',
    'system'
);

-- Sport status
CREATE TYPE sport_status_enum AS ENUM (
    'active',
    'inactive',
    'suspended'
);

-- Bonus status
CREATE TYPE bonus_status_enum AS ENUM (
    'active',
    'expired',
    'cancelled',
    'completed'
);

-- Outbox status
CREATE TYPE outbox_status_enum AS ENUM (
    'pending',
    'sent',
    'failed'
);
