use tokio::sync::broadcast::error::RecvError;

use super::PlayEvent;
use crate::api::annotation::record_play;
use crate::db::users;
use crate::state::{AppState, JUKEBOX_PLAYER as PLAYER, NowPlaying};

pub fn spawn(st: AppState) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let mut events = st.jukebox.plays();
    handle.spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => handle_event(&st, event).await,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            }
        }
    });
}

async fn handle_event(st: &AppState, event: PlayEvent) {
    match event {
        PlayEvent::Stopped { user_id } => {
            st.now_playing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&(user_id, PLAYER.to_string()));
            st.live.notify();
        }
        PlayEvent::Started { user_id, song_id } => {
            let id = user_id.clone();
            let Ok(Some(user)) = st.db.run(move |c| Ok(users::by_id(c, &id)?)).await else {
                return;
            };
            st.now_playing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    (user_id.clone(), PLAYER.to_string()),
                    NowPlaying {
                        user_id: user_id.clone(),
                        song_id: song_id.clone(),
                        username: user.name.clone(),
                        player: PLAYER.into(),
                        started: crate::db::now(),
                        until: i64::MAX,
                    },
                );
            st.live.notify();
            crate::api::lastfm::forward(st, &user, vec![song_id], Vec::new(), false);
        }
        PlayEvent::Played {
            user_id,
            song_id,
            at,
        } => {
            let id = user_id.clone();
            let Ok(Some(user)) = st.db.run(move |c| Ok(users::by_id(c, &id)?)).await else {
                return;
            };
            if !user.scrobbling {
                return;
            }
            let (uid, sid) = (user.id.clone(), song_id.clone());
            let saved = st
                .db
                .run(move |c| Ok(record_play(c, &uid, &sid, at)?))
                .await;
            if saved.is_ok_and(|fresh| fresh) {
                crate::api::lastfm::forward(st, &user, vec![song_id], vec![at * 1000], true);
                st.live.notify();
            }
        }
    }
}
