-- Migration: 008_rls_and_triggers.sql
-- Description: RLS policies, audit triggers, Citus sharding
-- Author: DATA_ENGINEER
-- Date: 2026-03-24

-- ============================================================
-- ROW LEVEL SECURITY
-- ============================================================

-- Enable RLS on user-facing tables
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
ALTER TABLE wallets ENABLE ROW LEVEL SECURITY;
ALTER TABLE wallet_transactions ENABLE ROW LEVEL SECURITY;
ALTER TABLE bets ENABLE ROW LEVEL SECURITY;
ALTER TABLE user_preferences ENABLE ROW LEVEL SECURITY;
ALTER TABLE user_limits ENABLE ROW LEVEL SECURITY;

-- Users: can only see own record
CREATE POLICY users_own_data ON users
    FOR ALL
    USING (id = current_setting('app.current_user_id', true)::BIGINT);

-- Wallets: user can see own wallets
CREATE POLICY wallets_own_data ON wallets
    FOR SELECT
    USING (user_id = current_setting('app.current_user_id', true)::BIGINT);

-- Transactions: user can see own transactions
CREATE POLICY wtx_own_data ON wallet_transactions
    FOR SELECT
    USING (user_id = current_setting('app.current_user_id', true)::BIGINT);

-- Bets: user can see own bets
CREATE POLICY bets_own_data ON bets
    FOR SELECT
    USING (user_id = current_setting('app.current_user_id', true)::BIGINT);

-- Preferences: user can modify own preferences
CREATE POLICY prefs_own_data ON user_preferences
    FOR ALL
    USING (user_id = current_setting('app.current_user_id', true)::BIGINT);

-- Limits: user can see own limits
CREATE POLICY limits_own_data ON user_limits
    FOR ALL
    USING (user_id = current_setting('app.current_user_id', true)::BIGINT);

-- Bypass RLS for service accounts
-- Run as superuser or with BYPASSRLS role

-- ============================================================
-- AUDIT TRIGGERS ON CRITICAL TABLES
-- ============================================================

CREATE TRIGGER trg_audit_users
    AFTER INSERT OR UPDATE OR DELETE ON users
    FOR EACH ROW EXECUTE FUNCTION audit_trigger();

CREATE TRIGGER trg_audit_wallets
    AFTER INSERT OR UPDATE OR DELETE ON wallets
    FOR EACH ROW EXECUTE FUNCTION audit_trigger();

CREATE TRIGGER trg_audit_bets
    AFTER INSERT OR UPDATE OR DELETE ON bets
    FOR EACH ROW EXECUTE FUNCTION audit_trigger();

CREATE TRIGGER trg_audit_user_limits
    AFTER INSERT OR UPDATE OR DELETE ON user_limits
    FOR EACH ROW EXECUTE FUNCTION audit_trigger();

-- ============================================================
-- CITUS SHARDING
-- ============================================================
--
-- Guarded on purpose. `create_distributed_table` only exists when the Citus
-- extension is installed AND the current database is coordinator-managed.
-- A single-node Citus image (citusdata/citus) still needs the extension
-- created and the node registered, and a plain PostgreSQL instance (the
-- docker-compose dev stack, CI service containers, test databases) has no
-- Citus at all. Unguarded, this file aborted the whole migration run at the
-- first statement and left the schema half-created.
--
-- Sharding is a scale-out concern: it must not block a fresh install.

DO $$
DECLARE
    citus_available BOOLEAN;
    t               TEXT;
    distributed     TEXT[] := ARRAY[
        'users', 'wallets', 'wallet_transactions', 'bets',
        'bet_selections', 'user_preferences', 'user_limits'
    ];
    reference       TEXT[] := ARRAY[
        'currencies', 'countries', 'sports', 'game_configs', 'house_accounts'
    ];
BEGIN
    SELECT EXISTS (
        SELECT 1 FROM pg_extension WHERE extname = 'citus'
    ) INTO citus_available;

    IF NOT citus_available THEN
        RAISE NOTICE 'citus extension absent - skipping table distribution';
        RETURN;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_proc WHERE proname = 'create_distributed_table'
    ) THEN
        RAISE NOTICE 'create_distributed_table() absent - skipping table distribution';
        RETURN;
    END IF;

    FOREACH t IN ARRAY distributed LOOP
        IF to_regclass(t) IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM pg_dist_partition WHERE logicalrelid = to_regclass(t))
        THEN
            -- Distributed by user_id for co-location; `users` is keyed by id.
            PERFORM create_distributed_table(
                t,
                CASE WHEN t = 'users' THEN 'id' ELSE 'user_id' END
            );
        END IF;
    END LOOP;

    FOREACH t IN ARRAY reference LOOP
        IF to_regclass(t) IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM pg_dist_partition WHERE logicalrelid = to_regclass(t))
        THEN
            PERFORM create_reference_table(t);
        END IF;
    END LOOP;
END
$$;

-- ============================================================
-- HELPER: set current user for RLS
-- ============================================================

CREATE OR REPLACE FUNCTION set_current_user(p_user_id BIGINT)
RETURNS VOID AS $$
BEGIN
    PERFORM set_config('app.current_user_id', p_user_id::TEXT, true);
END;
$$ LANGUAGE plpgsql;
