//! Third-party integrations: the outbound side of Atlas.
//!
//! Each integration is a self-contained module that Atlas *calls out to* (or, for
//! webhooks, is called back by). They share nothing but the [`crate::secrets`]
//! vault they draw their credentials from and the [`crate::error`] taxonomy they
//! report through.
//!
//! - [`github`] — repository linking, the card→branch→PR flow, the webhook
//!   receiver, and smart commits (`TODO.md` Phase 12).

pub mod github;
