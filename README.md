<div align="center">

<img width="128px" alt="Juuz" src="https://github.com/user-attachments/assets/00785b98-e5cd-4f7c-a1fd-c0b179b864e6" />

<h1><b>juuz</b></h1>

An OpenSubsonic server written in Rust with song uploads and OpenMusicMetadata tagging.

</div>
<br>

<img width="2560" height="1440" alt="Screenshot 2026-10-17 at 04-10-52 Juuz" src="https://github.com/user-attachments/assets/5d803ea0-2a92-471f-ad27-c74b05217a13" />

## Features

- **[Song upload](#song-upload)**: apps can save downloaded music straight to the server instead of the device, with covers, lyrics and tag fixing included
- OpenSubsonic API 1.16.1, JSON and XML, tested with Feishin and Arpeggi
- Every login style works for every user: password, token, API key
- Song and artist matching through [OpenMusicMetadata](https://docs.vleer.app) exposed as `ommId`, missing years, genres and artwork filled in without touching the files
- Songs identified by audio, so renaming or moving files keeps stars, playlists and history
- Last.fm scrobbling and now playing, linked per user
- Embedded lyrics and `.lrc` files first, then synced lyrics from LRCLIB
- Albums and compilations grouped correctly without an album artist tag
- Live SQLite library that watches the music folders
- Opus, MP3 and AAC transcoding (`format=opus`, `mp3`, `aac`) in pure Rust, no ffmpeg
- Podcasts, shares, internet radio, play queue, bookmarks
- Jukebox: the server plays to a DLNA speaker and any `jukeboxControl` app (Feishin) acts as the remote
- **[Web UI](#web-ui)**: a keyboard-driven terminal-style interface for stats, account settings and user management

## Installation

Save this as `docker-compose.yml`, point `/music` at the music folder and run `docker compose up -d`.

```yaml
services:
  juuz:
    image: ghcr.io/vleerapp/juuz:0.2.0
    restart: unless-stopped
    ports:
      - "4533:4533"
    environment:
      JUUZ_ADMIN_PASSWORD: replace-with-a-long-random-password
      JUUZ_OMM: "true"
      JUUZ_LASTFM_API_KEY: ""
      JUUZ_LASTFM_SECRET: ""
      JUUZ_PUBLIC_URL: "https://juuz.example.com"
      JUUZ_TRUST_PROXY: true
    volumes:
      - data:/data
      - /path/to/your/music:/music

volumes:
  data:
```

The server runs on port 4533. The music folder must be writable by uid 1000, since uploads are saved there.

## Web UI

Open `http://<host>:4533` and sign in with any juuz user. There is no player. Pages:

- `stats`: play stats (top songs, albums and artists, recent plays, plays per day), per user or for everyone as admin
- `account`: username, max bitrate, play recording, password, Last.fm link
- `keys`: API keys
- `users` (admin): users and roles
- `server` (admin): library scan and a read-only view of the server configuration

## Configuration

Environment variables, set in `docker-compose.yml`:

| Variable              | Default             | Description                                                  |
| --------------------- | ------------------- | ------------------------------------------------------------ |
| `JUUZ_ADMIN_PASSWORD` | random, logged once | Password of the `admin` user, only used on first start       |
| `JUUZ_PUBLIC_URL`     | from request        | Public base url used in share links, e.g. `https://music.example.com` |
| `JUUZ_RATE_LIMIT`     | `1800`              | Requests per minute per client ip (burst 300), `0` turns it off. 10 failed logins from one ip lock that ip out for 5 minutes |
| `JUUZ_TRUST_PROXY`    | `false`             | Set `true` behind a reverse proxy so limits use the real client ip from `X-Forwarded-For` |
| `JUUZ_OMM`            | `true`              | Set `false` to stop sending track names to OpenMusicMetadata |
| `JUUZ_LASTFM_API_KEY` |                     | Last.fm API key, scrobbling is off without it                |
| `JUUZ_LASTFM_SECRET`  |                     | Last.fm shared secret                                        |

## Last.fm

Create an API account at [last.fm/api](https://www.last.fm/api/account/create), set both variables and restart. Each user links their account once: call `juuzLastfmAuthUrl`, open the returned `url` and allow access, then call `juuzLastfmLink?token=<token>`. After that every `scrobble` from any app is forwarded as now playing and as a scrobble.

## Song upload

An app sends a file to `juuzUploadSong` with an API key (`juuzCreateApiKey`). The server writes the tags, saves the file and indexes it right away.

```sh
curl -F file=@track.flac \
  "http://localhost:4533/rest/juuzUploadSong?f=json&apiKey=$KEY&title=Song&artist=Artist&album=Album&track=1&year=2020"
```

```
music/
└── Artist/
    └── 2020 - Album/
        └── 01 - Song.flac
```

Tags can be set with `title`, `artist`, `albumArtist`, `album`, `track`, `disc`, `year` and `genre`. Missing ones are filled from `relativePath` (the original path) or `ommId` (an OpenMusicMetadata id). Add `kind=cover&albumId=…` or `kind=lyrics&songId=…` for covers and `.lrc` files, and call `juuzDeleteSong?id=…` to delete a song. Uploading the same song twice returns `"status": "duplicate"`. The user needs the upload role.