-- Messages for sessions on machines that are online, held until the machine
-- collects them. A machine is online while it polls; each poll records the
-- sessions running there. A null sender is the owner, writing in the reader.
ALTER TABLE sources
    ADD COLUMN polled_at timestamptz,
    ADD COLUMN running text[] NOT NULL DEFAULT '{}';

CREATE TABLE deliveries (
    id uuid PRIMARY KEY,
    source_id uuid NOT NULL REFERENCES sources(id),
    session_id text NOT NULL,
    text text NOT NULL CHECK (octet_length(text) BETWEEN 1 AND 65536),
    sender_source_id uuid REFERENCES sources(id),
    sender_session_id text,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((sender_source_id IS NULL) = (sender_session_id IS NULL))
);

CREATE INDEX deliveries_pending ON deliveries (source_id, created_at);
