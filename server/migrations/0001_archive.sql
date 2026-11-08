CREATE TABLE sources (
    id uuid PRIMARY KEY,
    label text NOT NULL,
    token_hash bytea NOT NULL CHECK (octet_length(token_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz,
    revoked_at timestamptz
);

CREATE TABLE sessions (
    source_id uuid NOT NULL REFERENCES sources(id),
    session_id text NOT NULL,
    revision bigint NOT NULL CHECK (revision > 0),
    format text NOT NULL CHECK (format = 'opencode.v2'),
    info json,
    info_hash bytea CHECK (octet_length(info_hash) = 32),
    upstream_version text,
    collector_version text,
    captured_at timestamptz,
    received_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    PRIMARY KEY (source_id, session_id),
    CHECK (
        (deleted_at IS NULL AND info IS NOT NULL AND info_hash IS NOT NULL
            AND upstream_version IS NOT NULL AND collector_version IS NOT NULL
            AND captured_at IS NOT NULL)
        OR (deleted_at IS NOT NULL AND info IS NULL AND info_hash IS NULL)
    )
);

CREATE TABLE messages (
    source_id uuid NOT NULL,
    session_id text NOT NULL,
    message_id text NOT NULL,
    position integer NOT NULL CHECK (position >= 0),
    payload json NOT NULL,
    payload_hash bytea NOT NULL CHECK (octet_length(payload_hash) = 32),
    PRIMARY KEY (source_id, session_id, message_id),
    FOREIGN KEY (source_id, session_id) REFERENCES sessions(source_id, session_id) ON DELETE CASCADE,
    UNIQUE (source_id, session_id, position) DEFERRABLE INITIALLY DEFERRED
);
