CREATE TABLE play_events (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL CHECK (event_type IN ('PLAY', 'STOP', 'PAUSE', 'RESUME', 'SEEK')),
    song_id TEXT NOT NULL,
    playlist_id TEXT,
    position INTEGER,
    timestamp TEXT NOT NULL DEFAULT (STRFTIME('%Y-%m-%d %H:%M:%f', 'now'))
);
CREATE INDEX play_events_user_time ON play_events(user_id, timestamp);
CREATE INDEX play_events_song ON play_events(song_id);
CREATE INDEX play_events_type ON play_events(event_type);

INSERT INTO play_events(id, user_id, event_type, song_id, position, timestamp)
SELECT lower(hex(randomblob(12))), sc.user_id, 'PLAY', sc.song_id, 0,
       STRFTIME('%Y-%m-%d %H:%M:%f', sc.played_at, 'unixepoch')
FROM scrobbles sc;

INSERT INTO play_events(id, user_id, event_type, song_id, position, timestamp)
SELECT lower(hex(randomblob(12))), sc.user_id, 'STOP', sc.song_id,
       COALESCE(s.duration_ms, 0),
       STRFTIME('%Y-%m-%d %H:%M:%f', sc.played_at + COALESCE(s.duration_ms, 0) / 1000.0, 'unixepoch')
FROM scrobbles sc LEFT JOIN songs s ON s.id = sc.song_id;
