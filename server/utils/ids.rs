use xxhash_rust::xxh3::xxh3_128;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Artist,
    Album,
    Song,
    Genre,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Kind::Artist => "artist",
            Kind::Album => "album",
            Kind::Song => "song",
            Kind::Genre => "genre",
        }
    }
}

pub fn fold_key(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

fn cuid_shaped(mut n: u128) -> String {
    let mut out = [b'0'; 24];
    for slot in out.iter_mut().rev() {
        *slot = DIGITS[(n % 36) as usize];
        n /= 36;
    }
    if out[0].is_ascii_digit() {
        out[0] = b'a' + (out[0] - b'0');
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn make(kind: Kind, parts: &[&str]) -> String {
    let mut buf = Vec::new();
    buf.extend_from_slice(kind.tag().as_bytes());
    for p in parts {
        buf.push(0x1f);
        buf.extend_from_slice(p.as_bytes());
    }
    cuid_shaped(xxh3_128(&buf))
}

pub fn artist(name: &str) -> String {
    make(Kind::Artist, &[&fold_key(name)])
}

pub fn album(key: &[&str]) -> String {
    make(Kind::Album, key)
}

pub fn song(audio_hash: &str, album_id: &str) -> String {
    make(Kind::Song, &[audio_hash, album_id])
}

pub fn genre(name: &str) -> String {
    make(Kind::Genre, &[&fold_key(name)])
}

pub fn random() -> String {
    cuid2::create_id()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_cuid_shaped() {
        let a = artist("Daft Punk");
        assert_eq!(a, artist("  daft   punk "));
        assert_eq!(a.len(), 24);
        assert!(a.chars().next().unwrap().is_ascii_lowercase());
        assert!(
            a.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
        assert_ne!(a, album(&["daft punk"]));
    }

    #[test]
    fn random_ids_are_cuid2() {
        let id = random();
        assert_eq!(id.len(), 24);
        assert_ne!(id, random());
    }
}
