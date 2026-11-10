-- Device login: a machine requests a code, the owner approves it in the reader,
-- and the machine then collects a new source token once. Only a hash of the
-- machine's device code is stored; the token is created when it is collected.
CREATE TABLE device_logins (
    device_code_hash bytea PRIMARY KEY CHECK (octet_length(device_code_hash) = 32),
    user_code text NOT NULL UNIQUE CHECK (user_code COLLATE "C" ~ '^[BCDFGHJKLMNPQRSTVWXZ]{8}$'),
    alias text NOT NULL CHECK (alias COLLATE "C" ~ '^[a-z][a-z0-9-]{0,31}$'),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    decided_at timestamptz,
    approved boolean,
    source_id uuid REFERENCES sources(id),
    CHECK ((decided_at IS NULL) = (approved IS NULL)),
    CHECK (source_id IS NULL OR approved)
);
