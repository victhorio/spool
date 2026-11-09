-- Run once after migrations, as the archive/database owner with CREATEROLE.
-- An existing spool_reader is an error, not an identity to repurpose.
-- PUBLIC must not grant archive writes, sensitive reads, or public schema CREATE.
-- Password assignment and enabling LOGIN are separate operator steps.
BEGIN;

CREATE ROLE spool_reader
    NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;

ALTER ROLE spool_reader SET default_transaction_read_only = on;

DO $$
BEGIN
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO spool_reader', current_database());
END
$$;

GRANT USAGE ON SCHEMA public TO spool_reader;
GRANT SELECT (id, label) ON public.sources TO spool_reader;
GRANT SELECT (source_id, session_id, revision, info, received_at, deleted_at)
    ON public.sessions TO spool_reader;
GRANT SELECT (source_id, session_id, position, payload) ON public.messages TO spool_reader;

COMMIT;
