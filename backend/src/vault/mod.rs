//! The secrets vault: encryption at rest for third-party API credentials.
//!
//! ## Shape
//!
//! - [`secret`] — [`secret::Secret<T>`], the zeroizing, un-loggable wrapper any
//!   plaintext credential material is constructed into and never leaves.
//! - [`crypto`] — [`crypto::Crypto`], the XChaCha20-Poly1305 cipher derived
//!   from the instance master key via HKDF-SHA256.
//! - [`credential`] — the `api_credentials` row, its repository, and the
//!   redacted DTO the API is allowed to return.
//! - [`github`] — the GitHub PAT validation probe.
//!
//! The HTTP handlers live in [`crate::api::vault`]: this module is the domain,
//! `api` is the surface — the same split [`crate::auth`] uses.
//!
//! ## Non-negotiable
//!
//! Plaintext credential material exists only as a [`secret::Secret`], only for
//! the duration of a seal or a validation probe, and is never logged,
//! `Debug`-printed, serialized, or returned over the API. See `CLAUDE.md`.

pub mod credential;
pub mod crypto;
pub mod github;
pub mod secret;

pub use crypto::Crypto;
pub use secret::Secret;
