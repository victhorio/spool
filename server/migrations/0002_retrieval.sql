CREATE EXTENSION IF NOT EXISTS pg_trgm;

ALTER TABLE sessions
    ADD COLUMN retrieval_title text,
    ADD COLUMN repository text,
    ADD COLUMN activity bigint;

ALTER TABLE messages
    ADD COLUMN retrieval_role text,
    ADD COLUMN retrieval_text text,
    ADD COLUMN conversation_text text,
    ADD COLUMN tool_text text,
    ADD COLUMN conversation_spans jsonb,
    ADD COLUMN tool_spans jsonb;

CREATE INDEX messages_conversation_trgm ON messages USING gin (conversation_text gin_trgm_ops);
CREATE INDEX messages_conversation_tools_trgm ON messages
    USING gin ((conversation_text || E'\n' || tool_text) gin_trgm_ops);
CREATE INDEX sessions_retrieval_recent ON sessions (activity DESC, source_id, session_id)
    WHERE retrieval_title IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX sessions_retrieval_repository_recent ON sessions
    (repository, activity DESC, source_id, session_id)
    WHERE retrieval_title IS NOT NULL AND deleted_at IS NULL;
