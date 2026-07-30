//! The AEAD primitive behind the vault: XChaCha20-Poly1305 with an HKDF-derived
//! key.
//!
//! # Why XChaCha20-Poly1305
//!
//! Its nonce is 192 bits, so a randomly chosen nonce is safe *forever* — the
//! birthday bound is irrelevant at any scale Atlas will ever reach, and there is
//! no per-key message counter to keep, no sequence to persist, nothing to get
//! wrong. AES-GCM's 96-bit nonce makes random selection a real reuse risk, and
//! AES without hardware AES-NI is both slower and harder to keep constant-time.
//! See `docs/research/rust-stack.md` § encryption.
//!
//! # Why HKDF, and why the master key is never used directly
//!
//! The vault key is *derived* from `config.master_key` with HKDF-SHA256 under a
//! fixed context string, rather than the master key being handed to the cipher
//! as-is. Two payoffs:
//!
//! - the master key can key other purposes later (session-cookie keys, a
//!   webhook-signing key) by expanding it under a different `info` string, with
//!   no chance of the two subkeys colliding;
//! - the master key need not be exactly 32 bytes. HKDF-Extract accepts input
//!   keying material of any length and produces a uniform 32-byte subkey, so a
//!   longer or shorter base64 key is not a footgun.
//!
//! HKDF rather than Argon2 deliberately: Argon2 is for stretching *low-entropy*
//! passwords, and a base64 master key from the environment is already uniform, so
//! a memory-hard KDF buys nothing but latency.

use anyhow::{Context, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

use crate::secrets::Secret;

/// The key-derivation version that [`Crypto::from_master`] produces.
///
/// Stored on every row Atlas encrypts (`api_credentials.key_version`), so a
/// future master-key rotation can re-encrypt old rows while new rows use the new
/// version — the reader knows which key a ciphertext was sealed with. Bumping the
/// KDF context strings below must bump this in lock-step.
pub const KEY_VERSION: i64 = 1;

/// The 24-byte XChaCha20 nonce length. Named so the row-length checks read.
pub const NONCE_LEN: usize = 24;

/// HKDF salt. Doubles as the derivation namespace, so a future purpose that
/// expands the same master key under a different `info` cannot collide with this
/// one.
const HKDF_SALT: &[u8] = b"atlas.hkdf.v1";

/// HKDF `info`: the specific subkey being derived. Changing this string changes
/// the key, which is why [`KEY_VERSION`] is tied to it.
const HKDF_INFO: &[u8] = b"atlas.api-credentials.aead-key.v1";

/// The nonce and ciphertext produced by sealing a plaintext.
///
/// `nonce || ciphertext` could be stored as one BLOB; they are kept as two
/// columns instead so the schema documents itself and a future format is not
/// forced to reverse-engineer where the nonce ends.
#[derive(Debug, Clone)]
pub struct Sealed {
    /// The 24-byte random nonce this message was sealed under.
    pub nonce: Vec<u8>,
    /// The Poly1305-authenticated ciphertext (plaintext length + a 16-byte tag).
    pub ciphertext: Vec<u8>,
    /// Which [`KEY_VERSION`] sealed it.
    pub key_version: i64,
}

/// The vault's symmetric cipher, keyed from the master key.
///
/// Holds no plaintext and no copy of the master key — only the derived cipher —
/// and its [`std::fmt::Debug`] is redacted regardless, so it can never print key
/// material into a log.
pub struct Crypto {
    cipher: XChaCha20Poly1305,
    key_version: i64,
}

impl std::fmt::Debug for Crypto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Crypto")
            .field("key_version", &self.key_version)
            .field("cipher", &"[REDACTED]")
            .finish()
    }
}

impl Crypto {
    /// Derives the vault cipher from raw master-key bytes.
    ///
    /// The master bytes are consumed and scrubbed by the caller
    /// ([`Crypto::from_master_b64`]); this function copies nothing of them beyond
    /// the HKDF extract step, and the derived key lives only inside a
    /// [`Zeroizing`] buffer that is wiped as this function returns.
    pub fn from_master(master: &[u8]) -> anyhow::Result<Self> {
        if master.is_empty() {
            return Err(anyhow!("the master key is empty"));
        }

        let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), master);
        let mut key = Zeroizing::new([0u8; 32]);
        hk.expand(HKDF_INFO, key.as_mut())
            .map_err(|_| anyhow!("HKDF could not derive the vault key"))?;

        let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
            .map_err(|_| anyhow!("the derived vault key was the wrong length"))?;

        Ok(Self {
            cipher,
            key_version: KEY_VERSION,
        })
    }

    /// Derives the vault cipher from the base64 master key as configured.
    ///
    /// The decoded bytes are held in a [`Zeroizing`] buffer and wiped before
    /// return, so the raw key does not linger on the heap after the cipher exists.
    pub fn from_master_b64(b64: &str) -> anyhow::Result<Self> {
        let mut master = Zeroizing::new(
            BASE64
                .decode(b64.trim().as_bytes())
                .context("ATLAS_MASTER_KEY is not valid base64")?,
        );
        let crypto = Self::from_master(&master)?;
        master.zeroize();
        Ok(crypto)
    }

    /// The [`KEY_VERSION`] this cipher seals with.
    pub fn key_version(&self) -> i64 {
        self.key_version
    }

    /// Encrypts `plaintext`, binding the ciphertext to `aad`.
    ///
    /// A fresh random nonce is drawn per call — safe to do forever at this nonce
    /// width. `aad` (additional authenticated data) is *not* encrypted but *is*
    /// authenticated: a ciphertext sealed under one `aad` will not open under
    /// another, which is how the caller binds a ciphertext to the row it belongs
    /// to so it cannot be lifted onto a different one.
    pub fn seal(&self, plaintext: &[u8], aad: &[u8]) -> anyhow::Result<Sealed> {
        // `try_generate` rather than `generate`: the latter panics if the system
        // RNG fails, and a panic in a request handler is an outage. Here it
        // becomes a 500 with the cause logged.
        let nonce =
            XNonce::try_generate().map_err(|err| anyhow!("failed to generate a nonce: {err}"))?;

        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| anyhow!("failed to encrypt the secret"))?;

        Ok(Sealed {
            nonce: nonce.to_vec(),
            ciphertext,
            key_version: self.key_version,
        })
    }

    /// Decrypts a ciphertext sealed by [`Crypto::seal`] under the same `aad`.
    ///
    /// Returns the plaintext already wrapped in a [`Secret`], so the caller cannot
    /// accidentally log or serialise it, and the intermediate decrypt buffer is
    /// scrubbed before this returns.
    ///
    /// # Errors
    ///
    /// Fails if the nonce is not [`NONCE_LEN`] bytes, if the authentication tag
    /// does not verify — which happens when the ciphertext was tampered with,
    /// when a *different* master key is in use, or when the `aad` does not match
    /// the one it was sealed under — or if the plaintext is not valid UTF-8.
    pub fn open(
        &self,
        nonce: &[u8],
        ciphertext: &[u8],
        aad: &[u8],
    ) -> anyhow::Result<Secret<String>> {
        if nonce.len() != NONCE_LEN {
            return Err(anyhow!(
                "stored nonce is {} bytes, expected {NONCE_LEN}",
                nonce.len()
            ));
        }
        let nonce = XNonce::try_from(nonce).map_err(|_| anyhow!("stored nonce is malformed"))?;

        let mut plaintext = self
            .cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            // Deliberately opaque: "the tag did not verify" is all the caller may
            // learn, and which of the several causes it was is not their business.
            .map_err(|_| anyhow!("decryption failed: the ciphertext did not authenticate"))?;

        // Copy into the Secret, then scrub the decrypt buffer. The copy is
        // unavoidable — String owns its own allocation — but the loose Vec must
        // not outlive this call holding a second plaintext copy.
        let text = std::str::from_utf8(&plaintext)
            .map_err(|_| anyhow!("decrypted secret was not valid UTF-8"))?
            .to_owned();
        plaintext.zeroize();

        Ok(Secret::new(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A base64 master key for tests. Any bytes work; HKDF makes the length
    /// irrelevant.
    fn crypto() -> Crypto {
        Crypto::from_master_b64("dGhpcy1pcy1hLTMyLWJ5dGUtdGVzdC1tYXN0ZXIta2V5MDA=").unwrap()
    }

    #[test]
    fn seal_then_open_round_trips() {
        let c = crypto();
        let aad = b"atlas.credential.v1:row-1";
        let sealed = c.seal(b"ghp_supersecrettoken", aad).unwrap();

        assert_eq!(sealed.nonce.len(), NONCE_LEN);
        assert_eq!(sealed.key_version, KEY_VERSION);
        // The ciphertext is not the plaintext.
        assert_ne!(sealed.ciphertext, b"ghp_supersecrettoken");

        let opened = c.open(&sealed.nonce, &sealed.ciphertext, aad).unwrap();
        assert_eq!(opened.expose(), "ghp_supersecrettoken");
    }

    #[test]
    fn a_different_master_key_cannot_decrypt() {
        // The AEAD authenticity property: a ciphertext sealed under one key does
        // not open under another. Without it, a leaked database plus *any* key
        // would be a leaked token.
        let a = crypto();
        let b =
            Crypto::from_master_b64("YS10b3RhbGx5LWRpZmZlcmVudC0zMi1ieXRlLW1hc3Rlcmtl").unwrap();
        let aad = b"atlas.credential.v1:row-1";

        let sealed = a.seal(b"ghp_token", aad).unwrap();
        assert!(
            b.open(&sealed.nonce, &sealed.ciphertext, aad).is_err(),
            "a foreign key must not decrypt this ciphertext"
        );
    }

    #[test]
    fn flipping_one_ciphertext_byte_fails_the_tag() {
        // The Poly1305 tag: a single flipped bit must make decryption fail rather
        // than return a mangled plaintext.
        let c = crypto();
        let aad = b"atlas.credential.v1:row-1";
        let sealed = c.seal(b"ghp_token", aad).unwrap();

        let mut tampered = sealed.ciphertext.clone();
        tampered[0] ^= 0x01;
        assert!(
            c.open(&sealed.nonce, &tampered, aad).is_err(),
            "a tampered ciphertext must not authenticate"
        );
    }

    #[test]
    fn a_ciphertext_will_not_open_under_a_different_aad() {
        // The AAD binding, at the primitive level: the same ciphertext under a
        // different associated-data string must fail. This is what stops a
        // ciphertext being lifted from one row onto another.
        let c = crypto();
        let sealed = c.seal(b"ghp_token", b"atlas.credential.v1:row-1").unwrap();

        assert!(
            c.open(
                &sealed.nonce,
                &sealed.ciphertext,
                b"atlas.credential.v1:row-2"
            )
            .is_err(),
            "the AAD must be part of what the tag authenticates"
        );
    }

    #[test]
    fn a_wrong_length_nonce_is_rejected_rather_than_panicking() {
        let c = crypto();
        let sealed = c.seal(b"x", b"aad").unwrap();
        assert!(c.open(&[0u8; 8], &sealed.ciphertext, b"aad").is_err());
    }

    #[test]
    fn nonces_are_unique_across_seals() {
        // Random 192-bit nonces: two seals of the same plaintext must not collide
        // (and must produce different ciphertexts), which is what makes random
        // nonce selection safe without a counter.
        let c = crypto();
        let one = c.seal(b"same", b"aad").unwrap();
        let two = c.seal(b"same", b"aad").unwrap();
        assert_ne!(one.nonce, two.nonce);
        assert_ne!(one.ciphertext, two.ciphertext);
    }

    #[test]
    fn an_empty_master_key_is_refused() {
        assert!(Crypto::from_master(b"").is_err());
        assert!(Crypto::from_master_b64("").is_err());
    }

    #[test]
    fn a_non_base64_master_key_is_refused() {
        assert!(Crypto::from_master_b64("not valid base64!!!").is_err());
    }
}
