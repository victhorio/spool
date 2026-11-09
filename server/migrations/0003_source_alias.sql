ALTER TABLE sources
    ADD COLUMN alias text UNIQUE
        CHECK (alias COLLATE "C" ~ '^[a-z][a-z0-9-]{0,31}$');

CREATE INDEX sessions_native_id ON sessions (session_id, source_id)
    WHERE deleted_at IS NULL;
