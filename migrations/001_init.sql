CREATE TABLE users (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    token_secret BLOB,
    email TEXT NOT NULL DEFAULT '',
    admin_role INTEGER NOT NULL DEFAULT 0,
    settings_role INTEGER NOT NULL DEFAULT 1,
    stream_role INTEGER NOT NULL DEFAULT 1,
    download_role INTEGER NOT NULL DEFAULT 1,
    upload_role INTEGER NOT NULL DEFAULT 0,
    playlist_role INTEGER NOT NULL DEFAULT 1,
    cover_art_role INTEGER NOT NULL DEFAULT 1,
    comment_role INTEGER NOT NULL DEFAULT 0,
    podcast_role INTEGER NOT NULL DEFAULT 0,
    share_role INTEGER NOT NULL DEFAULT 0,
    jukebox_role INTEGER NOT NULL DEFAULT 0,
    scrobbling_enabled INTEGER NOT NULL DEFAULT 1,
    max_bit_rate INTEGER NOT NULL DEFAULT 0,
    lastfm_session BLOB,
    lastfm_name TEXT,
    created_at INTEGER NOT NULL
);

CREATE TABLE api_keys (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL DEFAULT '',
    key_hash TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER
);

CREATE TABLE music_folders (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL
);

CREATE TABLE artists (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    sort_name TEXT,
    mbid TEXT,
    image_hash TEXT,
    image_path TEXT,
    omm_art_url TEXT,
    omm_art_tried_at INTEGER,
    omm_id TEXT,
    omm_checked_at INTEGER,
    omm_retry_at INTEGER,
    created_at INTEGER NOT NULL
);
CREATE INDEX artists_name ON artists(name COLLATE NOCASE);

CREATE TABLE albums (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    sort_title TEXT,
    folder_id INTEGER REFERENCES music_folders(id) ON DELETE SET NULL,
    group_tier INTEGER NOT NULL DEFAULT 3,
    mbid TEXT,
    upc TEXT,
    label TEXT,
    year INTEGER,
    date TEXT,
    original_date TEXT,
    is_compilation INTEGER NOT NULL DEFAULT 0,
    derived_artist INTEGER NOT NULL DEFAULT 0,
    cover_hash TEXT,
    cover_path TEXT,
    cover_source TEXT,
    omm_art_url TEXT,
    omm_art_tried_at INTEGER,
    omm_id TEXT,
    omm_checked_at INTEGER,
    omm_retry_at INTEGER,
    created_at INTEGER NOT NULL
);
CREATE INDEX albums_title ON albums(title COLLATE NOCASE);

CREATE TABLE album_artists (
    album_id TEXT NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    artist_id TEXT NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY (album_id, artist_id)
);
CREATE INDEX album_artists_artist ON album_artists(artist_id);

CREATE TABLE songs (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    folder_id INTEGER NOT NULL REFERENCES music_folders(id) ON DELETE CASCADE,
    album_id TEXT NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    sort_title TEXT,
    track INTEGER,
    disc INTEGER,
    disc_subtitle TEXT,
    year INTEGER,
    date TEXT,
    original_date TEXT,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    bitrate INTEGER NOT NULL DEFAULT 0,
    sample_rate INTEGER NOT NULL DEFAULT 0,
    channels INTEGER NOT NULL DEFAULT 0,
    bit_depth INTEGER NOT NULL DEFAULT 0,
    size INTEGER NOT NULL DEFAULT 0,
    mtime INTEGER NOT NULL DEFAULT 0,
    suffix TEXT NOT NULL,
    content_type TEXT NOT NULL,
    audio_hash TEXT NOT NULL,
    mbid TEXT,
    mbid_track TEXT,
    isrc TEXT,
    bpm INTEGER,
    comment TEXT,
    composer TEXT,
    lyrics TEXT,
    rg_track_gain REAL,
    rg_track_peak REAL,
    rg_album_gain REAL,
    rg_album_peak REAL,
    cover_hash TEXT,
    display_artist TEXT NOT NULL DEFAULT '',
    omm_id TEXT,
    omm_checked_at INTEGER,
    omm_retry_at INTEGER,
    omm_synced_at INTEGER,
    seen_gen INTEGER NOT NULL DEFAULT 0,
    indexed_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX songs_album ON songs(album_id);
CREATE INDEX songs_audio_hash ON songs(audio_hash);
CREATE INDEX songs_folder ON songs(folder_id);
CREATE INDEX songs_seen_gen ON songs(seen_gen);
CREATE INDEX songs_title ON songs(title COLLATE NOCASE);
CREATE INDEX songs_cover ON songs(cover_hash);

CREATE TABLE song_artists (
    song_id TEXT NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
    artist_id TEXT NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY (song_id, artist_id)
);
CREATE INDEX song_artists_artist ON song_artists(artist_id);

CREATE TABLE genres (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE
);

CREATE TABLE song_genres (
    song_id TEXT NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
    genre_id TEXT NOT NULL REFERENCES genres(id) ON DELETE CASCADE,
    position INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (song_id, genre_id)
);
CREATE INDEX song_genres_genre ON song_genres(genre_id);

CREATE TABLE lyrics (
    song_id TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    content TEXT,
    fetched_at INTEGER NOT NULL
);

CREATE TABLE scan_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    gen INTEGER NOT NULL DEFAULT 0,
    last_scan_at INTEGER,
    last_scan_count INTEGER NOT NULL DEFAULT 0
);
INSERT INTO scan_state(id, gen) VALUES (1, 0);

CREATE VIRTUAL TABLE search_fts USING fts5(
    kind UNINDEXED,
    id UNINDEXED,
    text,
    tokenize = 'unicode61 remove_diacritics 2',
    prefix = '2 3'
);

CREATE TABLE starred (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    starred_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, item_id)
);

CREATE TABLE ratings (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    rating INTEGER NOT NULL,
    PRIMARY KEY (user_id, item_id)
);

CREATE TABLE play_stats (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    song_id TEXT NOT NULL,
    play_count INTEGER NOT NULL DEFAULT 0,
    last_played INTEGER NOT NULL,
    PRIMARY KEY (user_id, song_id)
);
CREATE INDEX play_stats_song ON play_stats(song_id);

CREATE TABLE scrobbles (
    id INTEGER PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    song_id TEXT NOT NULL,
    played_at INTEGER NOT NULL
);
CREATE INDEX scrobbles_user_played ON scrobbles(user_id, played_at);
CREATE INDEX scrobbles_song ON scrobbles(song_id);

CREATE TABLE web_sessions (
    token_hash BLOB PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
);
CREATE INDEX web_sessions_user ON web_sessions(user_id);

CREATE TABLE playlists (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    comment TEXT NOT NULL DEFAULT '',
    public INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    changed_at INTEGER NOT NULL
);

CREATE TABLE playlist_songs (
    playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    song_id TEXT NOT NULL,
    PRIMARY KEY (playlist_id, position)
);

CREATE TABLE play_queue (
    user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    current_id TEXT,
    current_index INTEGER,
    position_ms INTEGER NOT NULL DEFAULT 0,
    changed_at INTEGER NOT NULL,
    changed_by TEXT NOT NULL DEFAULT ''
);

CREATE TABLE play_queue_songs (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    song_id TEXT NOT NULL,
    PRIMARY KEY (user_id, position)
);

CREATE TABLE bookmarks (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    song_id TEXT NOT NULL,
    position_ms INTEGER NOT NULL,
    comment TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    changed_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, song_id)
);

CREATE TABLE shares (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    description TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    last_visited INTEGER,
    visit_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE share_entries (
    share_id TEXT NOT NULL REFERENCES shares(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    item_id TEXT NOT NULL,
    PRIMARY KEY (share_id, position)
);

CREATE TABLE radio_stations (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    stream_url TEXT NOT NULL,
    homepage_url TEXT
);

CREATE TABLE chat_messages (
    id INTEGER PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    message TEXT NOT NULL
);

CREATE TABLE podcast_channels (
    id TEXT PRIMARY KEY,
    url TEXT NOT NULL UNIQUE,
    title TEXT,
    description TEXT,
    image_url TEXT,
    status TEXT NOT NULL DEFAULT 'new',
    error TEXT,
    created_at INTEGER NOT NULL,
    refreshed_at INTEGER
);

CREATE TABLE podcast_episodes (
    id TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL REFERENCES podcast_channels(id) ON DELETE CASCADE,
    guid TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT,
    published_at INTEGER,
    duration_s INTEGER,
    enclosure_url TEXT NOT NULL,
    content_type TEXT,
    size INTEGER,
    status TEXT NOT NULL DEFAULT 'new',
    error TEXT,
    path TEXT,
    UNIQUE (channel_id, guid)
);
CREATE INDEX podcast_episodes_channel ON podcast_episodes(channel_id);
CREATE INDEX podcast_episodes_published ON podcast_episodes(published_at);
