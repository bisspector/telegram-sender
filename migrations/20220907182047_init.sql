CREATE TABLE IF NOT EXISTS tg_chat (
  id BIGINT PRIMARY KEY,
  name TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tg_user (
  id BIGINT,
  chat_id BIGINT,
  username TEXT,
  name TEXT NOT NULL,
  PRIMARY KEY(id, chat_id),
  CONSTRAINT fk_chat
    FOREIGN KEY(chat_id)
      REFERENCES tg_chat(id)
      ON DELETE CASCADE
      ON UPDATE CASCADE
);