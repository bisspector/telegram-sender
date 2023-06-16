-- Add migration script here
CREATE TABLE IF NOT EXISTS message_queue (
    id SERIAL PRIMARY KEY,
    chats BIGINT[],
    message TEXT,
    images TEXT[],
    datetime TEXT
)