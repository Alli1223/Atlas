//! XChaCha20-Poly1305 encryption for secrets at rest.
//!
//! # Why XChaCha20-Poly1305 over AES-GCM
//!
//! Its 192-bit nonce is safe to pick at random forever — the birthday bound
//! that makes random 96-bit AES-GCM nonces a real reuse risk at scale never
//! becomes a problem here. It is also fast without needing AES-NI hardware
//! support, which self-hosted deployments cannot assume.
//!
//! # Why HKDF, not Argon2, to derive the working key
//!
//! Argon2 is for stretching *low-entropy* secrets — passwords a human typed.
//! `ATLAS_MASTER_KEY` is already a uniformly random 32 bytes (`openssl rand
//! -base64 32`), so a memory-hard KDF buys nothing there and only adds
//! latency. HKDF-SHA256 instead derives a per-purpose subkey from the one
//! master key, salted with a versioned string — so the derivation can change
//! without rotating the master key itself.
//!
//! See `docs/research/rust-stack.md` §5, including the aead 0.6 API note:
//! `generate_key(&mut OsRng)` was removed in favour of `Key::generate()` /
//! `Nonce::generate()` via the `Generate` trait.

use std::fmt;

use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

/// HKDF salt. Versioned so the derivation can change later without touching
/// the master key itself.
const HKDF_SALT: &[u8] = b"atlas.vault.v1";

/// HKDF info string for the vault's one subkey.
const SUBKEY_INFO: &[u8] = b"api-credential-encryption";

/// A sealed secret: nonce and ciphertext, stored together as the vault's
/// `nonce` and `ciphertext` columns.
#[derive(Debug, Clone)]
pub struct Sealed {
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// Derives the vault's working key from the instance master key, and
/// seals/opens secrets with it.
pub struct Crypto {
    cipher: XChaCha20Poly1305,
}

/// Hand-written, not derived: the default `Debug` for a `RustCrypto` cipher
/// prints its internal key schedule, and that is key material.
impl fmt::Debug for Crypto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Crypto").finish_non_exhaustive()
    }
}

impl Crypto {
    /// Derives the vault key from the master key via HKDF-SHA256.
    ///
    /// # Errors
    ///
    /// If HKDF's expand step rejects the requested output length. This never
    /// happens for the fixed 32-byte key requested here — it exists so a
    /// malformed derivation fails loudly rather than producing a weak key.
    pub fn from_master(master: &[u8]) -> anyhow::Result<Self> {
        let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), master);
        let mut key_bytes = Zeroizing::new([0u8; 32]);
        hk.expand(SUBKEY_INFO, key_bytes.as_mut())
            .map_err(|_| anyhow::anyhow!("failed to derive the vault encryption key"))?;

        let key: Key = Key::from(*key_bytes);
        Ok(Self {
            cipher: XChaCha20Poly1305::new(&key),
        })
    }

    /// A vault key that exists only for this process's lifetime.
    ///
    /// For development, when `ATLAS_MASTER_KEY` is unset: the vault still
    /// needs a key to boot, but nothing sealed under this one survives a
    /// restart. Never used in production — `Config::validate` refuses to
    /// start there without a real master key.
    ///
    /// # Errors
    ///
    /// Only if HKDF's expand step fails, which cannot happen for a fixed
    /// 32-byte input — see [`Crypto::from_master`].
    pub fn ephemeral() -> anyhow::Result<Self> {
        let key = Key::generate();
        Self::from_master(&key)
    }

    /// Encrypts `plaintext`, binding `aad` (associated data — the row's
    /// identity) so a ciphertext copy-pasted onto a different row fails to
    /// decrypt rather than silently decrypting as if it belonged there.
    ///
    /// # Errors
    ///
    /// If the underlying cipher rejects the operation, which the `aead` crate
    /// deliberately keeps opaque to avoid a side-channel oracle.
    pub fn seal(&self, plaintext: &str, aad: &[u8]) -> anyhow::Result<Sealed> {
        let nonce = XNonce::generate();
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext.as_bytes(),
                    aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("failed to encrypt secret"))?;

        Ok(Sealed {
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    /// Decrypts a [`Sealed`] value, checking it against the same `aad` it was
    /// sealed with.
    ///
    /// # Errors
    ///
    /// If the ciphertext, nonce or AAD do not match — including a ciphertext
    /// genuinely moved from another row, which is exactly what AAD binding is
    /// for — or if `sealed.nonce` is not 24 bytes.
    pub fn open(&self, sealed: &Sealed, aad: &[u8]) -> anyhow::Result<Zeroizing<String>> {
        let nonce = XNonce::try_from(sealed.nonce.as_slice())
            .map_err(|_| anyhow::anyhow!("stored nonce is not 24 bytes"))?;

        let plaintext = self
            .cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &sealed.ciphertext,
                    aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("failed to decrypt secret"))?;

        let plaintext = String::from_utf8(plaintext)
            .map_err(|_| anyhow::anyhow!("decrypted secret was not valid UTF-8"))?;

        Ok(Zeroizing::new(plaintext))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crypto() -> Crypto {
        Crypto::from_master(&[7u8; 32]).unwrap()
    }

    #[test]
    fn seal_then_open_round_trips() {
        let crypto = crypto();
        let sealed = crypto
            .seal("ghp_supersecrettoken", b"credential:1")
            .unwrap();
        let opened = crypto.open(&sealed, b"credential:1").unwrap();
        assert_eq!(&*opened, "ghp_supersecrettoken");
    }

    #[test]
    fn the_nonce_is_not_reused_across_calls() {
        let crypto = crypto();
        let a = crypto.seal("same-plaintext", b"aad").unwrap();
        let b = crypto.seal("same-plaintext", b"aad").unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn a_ciphertext_moved_to_another_row_fails_to_decrypt() {
        let crypto = crypto();
        let sealed = crypto.seal("ghp_token", b"credential:1").unwrap();
        // Same key, same nonce, same ciphertext — only the AAD (row identity)
        // differs, exactly as if the row were copy-pasted in the database.
        assert!(crypto.open(&sealed, b"credential:2").is_err());
    }

    #[test]
    fn a_different_master_key_cannot_decrypt() {
        let sealed = crypto().seal("ghp_token", b"credential:1").unwrap();
        let other = Crypto::from_master(&[9u8; 32]).unwrap();
        assert!(other.open(&sealed, b"credential:1").is_err());
    }

    #[test]
    fn debug_never_prints_key_material() {
        let rendered = format!("{:?}", crypto());
        assert_eq!(rendered, "Crypto { .. }");
    }
}
