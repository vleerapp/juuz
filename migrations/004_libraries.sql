CREATE TABLE libraries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
    path TEXT NOT NULL UNIQUE,
    writable INTEGER NOT NULL DEFAULT 1,
    scan_interval INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

INSERT INTO libraries(id, name, path, writable, created_at)
VALUES (1, 'Music', '', 1, CAST(strftime('%s', 'now') AS INTEGER));

CREATE TABLE user_libraries (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    library_id INTEGER NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, library_id)
);
CREATE INDEX user_libraries_library ON user_libraries(library_id);

INSERT INTO user_libraries(user_id, library_id) SELECT id, 1 FROM users;

ALTER TABLE songs ADD COLUMN library_id INTEGER NOT NULL DEFAULT 1;
ALTER TABLE albums ADD COLUMN library_id INTEGER NOT NULL DEFAULT 1;
ALTER TABLE users ADD COLUMN upload_library_id INTEGER;

CREATE INDEX songs_library ON songs(library_id);
CREATE INDEX albums_library ON albums(library_id);
