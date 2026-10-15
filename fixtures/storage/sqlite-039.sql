CREATE TABLE messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    msg_id TEXT,
    network TEXT NOT NULL,
    buffer TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    type TEXT NOT NULL,
    nick TEXT,
    text TEXT NOT NULL,
    highlight INTEGER DEFAULT 0,
    iv BLOB,
    ref_id TEXT,
    tags TEXT,
    event_key TEXT
);
CREATE UNIQUE INDEX idx_messages_msg_id ON messages(msg_id) WHERE msg_id IS NOT NULL;
CREATE VIRTUAL TABLE messages_fts USING fts5(nick, text, content=messages, content_rowid=id);
CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, nick, text) VALUES (new.id, new.nick, new.text);
END;
CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, nick, text) VALUES ('delete', old.id, old.nick, old.text);
END;
CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, nick, text) VALUES ('delete', old.id, old.nick, old.text);
    INSERT INTO messages_fts(rowid, nick, text) VALUES (new.id, new.nick, new.text);
END;
INSERT INTO messages(msg_id, network, buffer, timestamp, type, nick, text, tags)
VALUES ('legacy-local-id', 'fixture', '#history', 1700000000, 'message', 'Alice',
        'preserved legacy history', '{"msgid":"fixture-server-id"}');
