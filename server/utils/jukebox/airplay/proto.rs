use std::time::{SystemTime, UNIX_EPOCH};

use aes::Aes128;
use cbc::cipher::{BlockModeEncrypt, KeyIvInit};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hmac::{Hmac, Mac};
use num_bigint::BigUint;
use sha1::Sha1;
use sha2::{Digest, Sha512};

pub const TAG_LEN: usize = 16;
pub const MAX_FRAME: usize = 1024;

pub fn random<const N: usize>() -> [u8; N] {
    use rand::Rng;
    let mut out = [0u8; N];
    rand::rng().fill_bytes(&mut out);
    out
}

pub mod tlv {
    pub const METHOD: u8 = 0x00;
    pub const SALT: u8 = 0x02;
    pub const PUBLIC_KEY: u8 = 0x03;
    pub const PROOF: u8 = 0x04;
    pub const IDENTIFIER: u8 = 0x01;
    pub const ENCRYPTED: u8 = 0x05;
    pub const STATE: u8 = 0x06;
    pub const SIGNATURE: u8 = 0x0a;
    pub const ERROR: u8 = 0x07;
    pub const FLAGS: u8 = 0x13;

    const FRAGMENT: usize = 255;

    pub fn encode(items: &[(u8, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (tag, value) in items {
            if value.is_empty() {
                out.extend_from_slice(&[*tag, 0]);
            }
            for chunk in value.chunks(FRAGMENT) {
                out.push(*tag);
                out.push(chunk.len() as u8);
                out.extend_from_slice(chunk);
            }
        }
        out
    }

    pub struct Items(Vec<(u8, Vec<u8>)>);

    impl Items {
        pub fn get(&self, tag: u8) -> Option<&[u8]> {
            self.0
                .iter()
                .find(|(found, _)| *found == tag)
                .map(|(_, value)| value.as_slice())
        }
    }

    pub fn decode(bytes: &[u8]) -> Option<Items> {
        let mut items: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut continues = false;
        let mut rest = bytes;
        while !rest.is_empty() {
            let [tag, len, tail @ ..] = rest else {
                return None;
            };
            let len = usize::from(*len);
            let value = tail.get(..len)?;
            match items.last_mut() {
                Some((last, merged)) if continues && last == tag => merged.extend_from_slice(value),
                _ => items.push((*tag, value.to_vec())),
            }
            continues = len == FRAGMENT;
            rest = &tail[len..];
        }
        Some(Items(items))
    }
}

pub mod srp {
    use super::*;

    const PRIME_3072: &str = concat!(
        "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74",
        "020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F1437",
        "4FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED",
        "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF05",
        "98DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB",
        "9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B",
        "E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF695581718",
        "3995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33",
        "A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7",
        "ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864",
        "D87602733EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E2",
        "08E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF",
    );
    const GENERATOR: u32 = 5;
    const PRIME_BYTES: usize = 384;

    pub struct Exchange {
        pub public: Vec<u8>,
        pub proof: [u8; 64],
        pub server_proof: [u8; 64],
        pub key: [u8; 64],
    }

    fn hash(parts: &[&[u8]]) -> [u8; 64] {
        let mut hasher = Sha512::new();
        for part in parts {
            hasher.update(part);
        }
        hasher.finalize().into()
    }

    pub fn padded(number: &BigUint) -> Vec<u8> {
        let bytes = number.to_bytes_be();
        let mut out = vec![0; PRIME_BYTES.saturating_sub(bytes.len())];
        out.extend_from_slice(&bytes);
        out
    }

    pub fn prime() -> BigUint {
        BigUint::parse_bytes(PRIME_3072.as_bytes(), 16).expect("the SRP prime is valid hex")
    }

    pub fn exchange(
        user: &str,
        password: &str,
        salt: &[u8],
        server_public: &[u8],
        private: &[u8],
    ) -> Option<Exchange> {
        let prime = prime();
        let generator = BigUint::from(GENERATOR);
        let server = BigUint::from_bytes_be(server_public);
        if (&server % &prime) == BigUint::ZERO {
            return None;
        }
        let secret = BigUint::from_bytes_be(private);
        if secret == BigUint::ZERO {
            return None;
        }
        let public = generator.modpow(&secret, &prime);
        let multiplier = BigUint::from_bytes_be(&hash(&[&padded(&prime), &padded(&generator)]));
        let scrambler = BigUint::from_bytes_be(&hash(&[&padded(&public), &padded(&server)]));
        if scrambler == BigUint::ZERO {
            return None;
        }
        let identity = hash(&[format!("{user}:{password}").as_bytes()]);
        let x = BigUint::from_bytes_be(&hash(&[salt, &identity]));
        let subtrahend = (&multiplier * generator.modpow(&x, &prime)) % &prime;
        let base = ((&server % &prime) + &prime - subtrahend) % &prime;
        let shared = base.modpow(&(&secret + &scrambler * &x), &prime);
        let key = hash(&[&shared.to_bytes_be()]);
        let public = public.to_bytes_be();
        let proof = client_proof(&prime, user, salt, &public, &server.to_bytes_be(), &key);
        let server_proof = hash(&[&public, &proof, &key]);
        Some(Exchange {
            public,
            proof,
            server_proof,
            key,
        })
    }

    fn client_proof(
        prime: &BigUint,
        user: &str,
        salt: &[u8],
        client_public: &[u8],
        server_public: &[u8],
        key: &[u8],
    ) -> [u8; 64] {
        let prime_hash = hash(&[&prime.to_bytes_be()]);
        let generator_hash = hash(&[&BigUint::from(GENERATOR).to_bytes_be()]);
        let mut mixed = [0u8; 64];
        for (slot, (a, b)) in mixed
            .iter_mut()
            .zip(prime_hash.iter().zip(generator_hash.iter()))
        {
            *slot = a ^ b;
        }
        hash(&[
            &mixed,
            &hash(&[user.as_bytes()]),
            salt,
            client_public,
            server_public,
            key,
        ])
    }

    #[cfg(test)]
    pub struct Server {
        verifier: BigUint,
        secret: BigUint,
        pub public: BigUint,
        pub salt: Vec<u8>,
    }

    #[cfg(test)]
    impl Server {
        pub fn new(user: &str, password: &str, salt: &[u8], secret: &[u8]) -> Self {
            let prime = prime();
            let generator = BigUint::from(GENERATOR);
            let identity = hash(&[format!("{user}:{password}").as_bytes()]);
            let x = BigUint::from_bytes_be(&hash(&[salt, &identity]));
            let verifier = generator.modpow(&x, &prime);
            let multiplier = BigUint::from_bytes_be(&hash(&[&padded(&prime), &padded(&generator)]));
            let secret = BigUint::from_bytes_be(secret);
            let public = (&multiplier * &verifier + generator.modpow(&secret, &prime)) % &prime;
            Self {
                verifier,
                secret,
                public,
                salt: salt.to_vec(),
            }
        }

        pub fn accept(&self, client_public: &[u8], proof: &[u8]) -> Option<([u8; 64], [u8; 64])> {
            let prime = prime();
            let client = BigUint::from_bytes_be(client_public);
            let scrambler =
                BigUint::from_bytes_be(&hash(&[&padded(&client), &padded(&self.public)]));
            let shared =
                (&client * self.verifier.modpow(&scrambler, &prime)).modpow(&self.secret, &prime);
            let key = hash(&[&shared.to_bytes_be()]);
            let expected = client_proof(
                &prime,
                "Pair-Setup",
                &self.salt,
                &client.to_bytes_be(),
                &self.public.to_bytes_be(),
                &key,
            );
            (expected[..] == *proof).then(|| (key, hash(&[&client.to_bytes_be(), &expected, &key])))
        }
    }
}

pub fn derive(secret: &[u8], salt: &str, info: &str) -> [u8; 32] {
    let mut extract = <Hmac<Sha512> as KeyInit>::new_from_slice(salt.as_bytes())
        .expect("HMAC takes keys of any length");
    extract.update(secret);
    let prk = extract.finalize().into_bytes();
    let mut expand =
        <Hmac<Sha512> as KeyInit>::new_from_slice(&prk).expect("HMAC takes keys of any length");
    expand.update(info.as_bytes());
    expand.update(&[1]);
    let block = expand.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&block[..32]);
    out
}

fn tag_nonce(tag: &[u8; 8]) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(tag);
    nonce
}

pub fn seal_tagged(key: &[u8; 32], tag: &[u8; 8], plain: &[u8]) -> Vec<u8> {
    ChaCha20Poly1305::new(&Key::from(*key))
        .encrypt(
            &Nonce::from(tag_nonce(tag)),
            Payload {
                msg: plain,
                aad: &[],
            },
        )
        .expect("a short message always encrypts")
}

pub fn open_tagged(key: &[u8; 32], tag: &[u8; 8], sealed: &[u8]) -> Option<Vec<u8>> {
    ChaCha20Poly1305::new(&Key::from(*key))
        .decrypt(
            &Nonce::from(tag_nonce(tag)),
            Payload {
                msg: sealed,
                aad: &[],
            },
        )
        .ok()
}

fn counter_nonce(counter: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&counter.to_le_bytes());
    nonce
}

pub struct FrameCipher {
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl FrameCipher {
    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(&Key::from(*key)),
            counter: 0,
        }
    }

    pub fn seal(&mut self, plain: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(plain.len() + plain.len().div_ceil(MAX_FRAME) * 18);
        for chunk in plain.chunks(MAX_FRAME) {
            let length = (chunk.len() as u16).to_le_bytes();
            let sealed = self
                .cipher
                .encrypt(
                    &Nonce::from(counter_nonce(self.counter)),
                    Payload {
                        msg: chunk,
                        aad: &length,
                    },
                )
                .expect("a frame of at most 1024 bytes always encrypts");
            self.counter += 1;
            out.extend_from_slice(&length);
            out.extend_from_slice(&sealed);
        }
        out
    }

    pub fn open(&mut self, length: [u8; 2], sealed: &[u8]) -> Option<Vec<u8>> {
        let plain = self
            .cipher
            .decrypt(
                &Nonce::from(counter_nonce(self.counter)),
                Payload {
                    msg: sealed,
                    aad: &length,
                },
            )
            .ok()?;
        self.counter += 1;
        Some(plain)
    }
}

pub enum AudioCipher {
    None,
    ChaCha(ChaCha20Poly1305),
    AesCbc { key: [u8; 16], iv: [u8; 16] },
}

impl AudioCipher {
    pub fn new(key: &[u8; 32]) -> Self {
        Self::ChaCha(ChaCha20Poly1305::new(&Key::from(*key)))
    }

    pub fn aes_cbc(key: [u8; 16], iv: [u8; 16]) -> Self {
        Self::AesCbc { key, iv }
    }

    pub fn seal_packet(&self, header: &[u8; 12], seq: u16, payload: &[u8]) -> Vec<u8> {
        let mut packet = Vec::with_capacity(12 + payload.len() + TAG_LEN + 8);
        packet.extend_from_slice(header);
        match self {
            Self::None => packet.extend_from_slice(payload),
            Self::ChaCha(cipher) => {
                let mut nonce = [0u8; 12];
                nonce[4..6].copy_from_slice(&seq.to_le_bytes());
                let sealed = cipher
                    .encrypt(
                        &Nonce::from(nonce),
                        Payload {
                            msg: payload,
                            aad: &header[4..],
                        },
                    )
                    .expect("an audio packet always encrypts");
                packet.extend_from_slice(&sealed);
                packet.extend_from_slice(&nonce[4..]);
            }
            Self::AesCbc { key, iv } => {
                packet.extend_from_slice(payload);
                let mut encryptor = cbc::Encryptor::<Aes128>::new(key.into(), iv.into());
                for block in packet[12..].as_chunks_mut::<16>().0 {
                    encryptor.encrypt_block(block.into());
                }
            }
        }
        packet
    }
}

pub mod rsa {
    use super::*;

    const AIRPORT_MODULUS: &str = concat!(
        "E7D744F2A2E2788B6C1F55A08EB70544A8FA7945AA8BE6C62CE5F51CBDD4DC68",
        "42FE3D1083DD2EDEC1BFD4252DC02E6F398BDF0E6148EA84855E2E442DA6D626",
        "64F674A1F304929ADE4F6893EF2DF6E711A8C77A0D91C9D980822E50D12922AF",
        "EA40EA9F0E14C0F76938C5F3882FC0323DD9FE55155F51BB5921C201629FD733",
        "52D5E2EFAABF9BA048D7B813A2B6767F6C3CCF1EB4CE673D037B0D2EA30C5FFF",
        "EB06F8D08ADDE409571A9C689FEF10728855DD8CFB9A8BEF5C8943EF3B5FAA15",
        "DDE698BEDDF3599603EB3E6F61372BB628F6559F599A78BF500687AA7F4976C0",
        "562D412956F8989E18A6355BD81597825E0FC875343EC782117625CDBF98447B",
    );
    const EXPONENT: u32 = 65_537;
    const HASH_LEN: usize = 20;

    pub fn encrypt_for_airport(key: &[u8; 16]) -> Vec<u8> {
        let modulus = BigUint::parse_bytes(AIRPORT_MODULUS.as_bytes(), 16)
            .expect("the AirPort Express modulus is valid hex");
        oaep_encrypt(&modulus, key, random::<HASH_LEN>())
    }

    pub fn oaep_encrypt(modulus: &BigUint, message: &[u8], seed: [u8; HASH_LEN]) -> Vec<u8> {
        let size = modulus.bits().div_ceil(8) as usize;
        let mut block = vec![0u8; size - HASH_LEN - 1];
        block[..HASH_LEN].copy_from_slice(&Sha1::digest(b""));
        let start = block.len() - message.len();
        block[start - 1] = 1;
        block[start..].copy_from_slice(message);
        mask(&mut block, &seed);
        let mut masked_seed = seed;
        mask(&mut masked_seed, &block);
        let encoded = [&[0][..], &masked_seed, &block].concat();
        let sealed = BigUint::from_bytes_be(&encoded).modpow(&BigUint::from(EXPONENT), modulus);
        let bytes = sealed.to_bytes_be();
        let mut out = vec![0; size.saturating_sub(bytes.len())];
        out.extend_from_slice(&bytes);
        out
    }

    fn mask(target: &mut [u8], seed: &[u8]) {
        for (counter, chunk) in target.chunks_mut(HASH_LEN).enumerate() {
            let mut hasher = Sha1::new();
            hasher.update(seed);
            hasher.update((counter as u32).to_be_bytes());
            let digest = hasher.finalize();
            for (byte, mask) in chunk.iter_mut().zip(digest) {
                *byte ^= mask;
            }
        }
    }
}

pub mod rtp {
    use super::*;

    const NTP_EPOCH_OFFSET: u64 = 2_208_988_800;
    const PAYLOAD_ALAC: u8 = 0x60;
    const MARKER: u8 = 0x80;

    pub fn ntp_now() -> u64 {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let seconds = since.as_secs() + NTP_EPOCH_OFFSET;
        let fraction = (u64::from(since.subsec_nanos()) << 32) / 1_000_000_000;
        (seconds << 32) | fraction
    }

    pub fn ntp_add_micros(ntp: u64, micros: i64) -> u64 {
        let delta = ((micros.unsigned_abs() as u128) << 32) / 1_000_000;
        if micros >= 0 {
            ntp.wrapping_add(delta as u64)
        } else {
            ntp.wrapping_sub(delta as u64)
        }
    }

    pub fn audio_header(first: bool, seq: u16, timestamp: u32, ssrc: u32) -> [u8; 12] {
        let mut header = [0u8; 12];
        header[0] = 0x80;
        header[1] = if first {
            PAYLOAD_ALAC | MARKER
        } else {
            PAYLOAD_ALAC
        };
        header[2..4].copy_from_slice(&seq.to_be_bytes());
        header[4..8].copy_from_slice(&timestamp.to_be_bytes());
        header[8..12].copy_from_slice(&ssrc.to_be_bytes());
        header
    }

    pub fn sync_packet(first: bool, playing: u32, ntp: u64, next: u32) -> [u8; 20] {
        let mut packet = [0u8; 20];
        packet[0] = if first { 0x90 } else { 0x80 };
        packet[1] = 0xd4;
        packet[2..4].copy_from_slice(&7u16.to_be_bytes());
        packet[4..8].copy_from_slice(&playing.to_be_bytes());
        packet[8..16].copy_from_slice(&ntp.to_be_bytes());
        packet[16..20].copy_from_slice(&next.to_be_bytes());
        packet
    }

    pub fn timing_reply(request: &[u8], received: u64) -> Option<[u8; 32]> {
        if request.len() < 32 || request[1] & 0x7f != 0x52 {
            return None;
        }
        let mut reply = [0u8; 32];
        reply[0] = 0x80;
        reply[1] = 0xd3;
        reply[2..4].copy_from_slice(&7u16.to_be_bytes());
        reply[8..16].copy_from_slice(&request[24..32]);
        reply[16..24].copy_from_slice(&received.to_be_bytes());
        reply[24..32].copy_from_slice(&ntp_now().to_be_bytes());
        Some(reply)
    }

    pub fn resend_request(packet: &[u8]) -> Option<(u16, u16)> {
        if packet.len() < 8 || packet[1] & 0x7f != 0x55 {
            return None;
        }
        let first = u16::from_be_bytes([packet[4], packet[5]]);
        let count = u16::from_be_bytes([packet[6], packet[7]]);
        Some((first, count))
    }

    pub fn futile_resend(seq: u16) -> [u8; 8] {
        let [high, low] = seq.to_be_bytes();
        [0x80, 0xd6, 0x00, 0x01, high, low, 0, 0]
    }

    pub fn resend_packet(original: &[u8]) -> Vec<u8> {
        let mut packet = Vec::with_capacity(4 + original.len());
        packet.extend_from_slice(&[0x80, 0xd6, 0x00, 0x01]);
        packet.extend_from_slice(original);
        packet
    }
}

pub mod alac {
    struct BitWriter {
        bytes: Vec<u8>,
        bits: u32,
        filled: u8,
    }

    impl BitWriter {
        fn with_capacity(capacity: usize) -> Self {
            Self {
                bytes: Vec::with_capacity(capacity),
                bits: 0,
                filled: 0,
            }
        }

        fn put(&mut self, value: u32, count: u8) {
            for shift in (0..count).rev() {
                self.bits = (self.bits << 1) | ((value >> shift) & 1);
                self.filled += 1;
                if self.filled == 8 {
                    self.bytes.push(self.bits as u8);
                    self.bits = 0;
                    self.filled = 0;
                }
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.filled > 0 {
                self.bytes.push((self.bits << (8 - self.filled)) as u8);
            }
            self.bytes
        }
    }

    const ELEMENT_CPE: u32 = 1;
    const ELEMENT_END: u32 = 7;

    pub fn encode_uncompressed(stereo: &[i16]) -> Vec<u8> {
        let mut writer = BitWriter::with_capacity(stereo.len() * 2 + 8);
        writer.put(ELEMENT_CPE, 3);
        writer.put(0, 4);
        writer.put(0, 12);
        writer.put(0, 1);
        writer.put(0, 2);
        writer.put(1, 1);
        for sample in stereo {
            writer.put(u32::from(*sample as u16), 16);
        }
        writer.put(ELEMENT_END, 3);
        writer.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes_cbc_audio_decrypts_back_to_the_payload() {
        use cbc::cipher::BlockModeDecrypt;
        let (key, iv) = ([7u8; 16], [9u8; 16]);
        let header = [0x80u8; 12];
        let payload: Vec<u8> = (0..48u8).collect();
        let sealed = AudioCipher::aes_cbc(key, iv).seal_packet(&header, 1, &payload);
        assert_eq!(&sealed[..12], &header);
        assert_ne!(&sealed[12..], payload.as_slice());
        let mut plain = sealed[12..].to_vec();
        let mut decryptor = cbc::Decryptor::<Aes128>::new((&key).into(), (&iv).into());
        for block in plain.as_chunks_mut::<16>().0 {
            decryptor.decrypt_block(block.into());
        }
        assert_eq!(plain, payload);
    }

    #[test]
    fn tagged_messages_open_only_with_their_own_tag() {
        let key = [4u8; 32];
        let sealed = seal_tagged(&key, b"PV-Msg02", b"hello");
        assert_eq!(open_tagged(&key, b"PV-Msg02", &sealed).unwrap(), b"hello");
        assert!(open_tagged(&key, b"PV-Msg03", &sealed).is_none());
    }

    #[test]
    fn tlv_fragments_long_values_and_joins_them_back() {
        let long = vec![7u8; 600];
        let bytes = tlv::encode(&[(tlv::STATE, &[1]), (tlv::PUBLIC_KEY, &long)]);
        assert_eq!(bytes.len(), 3 + 3 * 2 + 600);
        let items = tlv::decode(&bytes).unwrap();
        assert_eq!(items.get(tlv::STATE), Some(&[1u8][..]));
        assert_eq!(items.get(tlv::PUBLIC_KEY), Some(&long[..]));
        assert!(tlv::decode(&[1, 9, 0]).is_none());
    }

    #[test]
    fn srp_client_and_server_agree_on_the_key_and_proofs() {
        let server = srp::Server::new("Pair-Setup", "3939", b"saltysaltysalty!", &[5; 32]);
        let exchange = srp::exchange(
            "Pair-Setup",
            "3939",
            &server.salt,
            &server.public.to_bytes_be(),
            &[9; 32],
        )
        .unwrap();
        let (key, proof) = server.accept(&exchange.public, &exchange.proof).unwrap();
        assert_eq!(key, exchange.key);
        assert_eq!(proof, exchange.server_proof);
        let wrong = srp::exchange(
            "Pair-Setup",
            "0000",
            &server.salt,
            &server.public.to_bytes_be(),
            &[9; 32],
        )
        .unwrap();
        assert!(server.accept(&wrong.public, &wrong.proof).is_none());
    }

    #[test]
    fn hkdf_matches_the_rfc_5869_vector() {
        let okm = {
            let mut extract = <Hmac<Sha512> as KeyInit>::new_from_slice(b"salt").unwrap();
            extract.update(b"secret");
            extract.finalize().into_bytes().to_vec()
        };
        assert_eq!(okm.len(), 64);
        let a = derive(b"secret", "salt", "info");
        assert_eq!(a, derive(b"secret", "salt", "info"));
        assert_ne!(a, derive(b"secret", "salt", "other"));
    }

    #[test]
    fn frames_open_in_order_and_a_tampered_frame_does_not() {
        let key = [9u8; 32];
        let plain: Vec<u8> = (0..2500).map(|i| i as u8).collect();
        let sealed = FrameCipher::new(&key).seal(&plain);
        assert_eq!(sealed.len(), plain.len() + 3 * (2 + TAG_LEN));
        let mut reader = FrameCipher::new(&key);
        let mut opened = Vec::new();
        let mut rest = sealed.as_slice();
        while !rest.is_empty() {
            let length = [rest[0], rest[1]];
            let size = usize::from(u16::from_le_bytes(length)) + TAG_LEN;
            opened.extend(reader.open(length, &rest[2..2 + size]).unwrap());
            rest = &rest[2 + size..];
        }
        assert_eq!(opened, plain);
        let mut tampered = FrameCipher::new(&key).seal(b"hello");
        tampered[3] ^= 1;
        let length = [tampered[0], tampered[1]];
        assert!(
            FrameCipher::new(&key)
                .open(length, &tampered[2..])
                .is_none()
        );
    }

    #[test]
    fn an_audio_packet_opens_with_the_trailing_nonce_and_header_aad() {
        let key = [3u8; 32];
        let header = rtp::audio_header(true, 0x1234, 0x01020304, 0x05060708);
        let payload: Vec<u8> = (0..40).collect();
        let packet = AudioCipher::new(&key).seal_packet(&header, 0x1234, &payload);
        assert_eq!(&packet[..12], &header);
        assert_eq!(packet.len(), 12 + 40 + TAG_LEN + 8);
        let nonce_tail = &packet[packet.len() - 8..];
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(nonce_tail);
        let opened = ChaCha20Poly1305::new(&Key::from(key))
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &packet[12..packet.len() - 8],
                    aad: &header[4..],
                },
            )
            .unwrap();
        assert_eq!(opened, payload);
    }

    #[test]
    fn raop_packets_encrypt_whole_blocks_and_leave_the_tail_clear() {
        let key: [u8; 16] = std::array::from_fn(|i| i as u8);
        let iv: [u8; 16] = std::array::from_fn(|i| 0xf0 + i as u8);
        let header = rtp::audio_header(false, 1, 1, 1);
        let payload: Vec<u8> = (0..40).collect();
        let cipher = AudioCipher::aes_cbc(key, iv);
        let packet = cipher.seal_packet(&header, 1, &payload);
        assert_eq!(packet.len(), 12 + 40);
        assert_ne!(packet[12..44], payload[..32]);
        assert_eq!(packet[44..], payload[32..]);
        assert_eq!(cipher.seal_packet(&header, 2, &payload), packet);
    }

    #[test]
    fn oaep_output_is_the_modulus_size() {
        let sealed = rsa::encrypt_for_airport(&[1; 16]);
        assert_eq!(sealed.len(), 256);
    }

    #[test]
    fn alac_marks_an_uncompressed_stereo_frame() {
        let frame = alac::encode_uncompressed(&[0x1234, -2]);
        assert_eq!(frame.len(), 3 + 4 + 1);
        assert_eq!(frame[0], 0b0010_0000);
        assert_eq!(frame[1], 0);
        assert_eq!(frame[2] & 0b1111_1110, 0b0000_0010);
    }

    #[test]
    fn sync_and_timing_packets_have_the_documented_layout() {
        let sync = rtp::sync_packet(true, 1000, 0x1122334455667788, 89_200);
        assert_eq!(&sync[..4], &[0x90, 0xd4, 0, 7]);
        assert_eq!(u32::from_be_bytes(sync[4..8].try_into().unwrap()), 1000);
        assert_eq!(u32::from_be_bytes(sync[16..20].try_into().unwrap()), 89_200);
        let mut request = [0u8; 32];
        request[1] = 0xd2;
        request[24..32].copy_from_slice(&42u64.to_be_bytes());
        let reply = rtp::timing_reply(&request, 99).unwrap();
        assert_eq!(&reply[..4], &[0x80, 0xd3, 0, 7]);
        assert_eq!(u64::from_be_bytes(reply[8..16].try_into().unwrap()), 42);
        assert_eq!(u64::from_be_bytes(reply[16..24].try_into().unwrap()), 99);
        assert!(rtp::timing_reply(&request[..20], 0).is_none());
        assert_eq!(
            rtp::resend_request(&[0x80, 0xd5, 0, 1, 0x12, 0x34, 0, 3]),
            Some((0x1234, 3))
        );
    }

    #[test]
    fn ntp_arithmetic_moves_by_microseconds() {
        let base = 100u64 << 32;
        assert_eq!(rtp::ntp_add_micros(base, 1_000_000), 101u64 << 32);
        assert_eq!(rtp::ntp_add_micros(base, -1_000_000), 99u64 << 32);
        assert!(rtp::ntp_now() >> 32 > 2_208_988_800 + 1_700_000_000);
    }
}
