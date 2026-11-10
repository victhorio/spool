-- Documents agents publish for people to read. Only the current version is kept.
-- Text types are stored as text, so they can be searched and edited in place;
-- raster images as bytes. session_id names the creating session, which need not
-- be archived yet, so it is not a foreign key.
CREATE TABLE artifacts (
    id text PRIMARY KEY CHECK (id COLLATE "C" ~ '^art_[a-z0-9]{12}$'),
    source_id uuid NOT NULL REFERENCES sources(id),
    session_id text,
    title text NOT NULL CHECK (octet_length(title) BETWEEN 1 AND 256),
    media_type text NOT NULL CHECK (media_type IN (
        'text/markdown', 'text/html', 'text/plain', 'image/svg+xml',
        'image/png', 'image/jpeg', 'image/gif', 'image/webp')),
    revision bigint NOT NULL CHECK (revision > 0),
    size integer NOT NULL CHECK (size BETWEEN 0 AND 33554432),
    text_content text,
    binary_content bytea,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((text_content IS NULL) <> (binary_content IS NULL)),
    CHECK ((text_content IS NOT NULL) = (media_type IN
        ('text/markdown', 'text/html', 'text/plain', 'image/svg+xml')))
);

-- Raster formats are already compressed. Uncompressed text lets a chunked read
-- fetch only the slices it needs instead of decompressing from the start.
ALTER TABLE artifacts
    ALTER COLUMN binary_content SET STORAGE EXTERNAL,
    ALTER COLUMN text_content SET STORAGE EXTERNAL;

CREATE INDEX artifacts_recent ON artifacts (updated_at DESC, id DESC);
CREATE INDEX artifacts_session ON artifacts (source_id, session_id);
CREATE INDEX artifacts_search ON artifacts
    USING gin ((title || E'\n' || coalesce(text_content, '')) gin_trgm_ops);
