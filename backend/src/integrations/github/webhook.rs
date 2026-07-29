//! The webhook receiver's two jobs: prove a delivery is really from GitHub, and
//! parse the events Atlas acts on.
//!
//! # Signature verification is the whole authentication
//!
//! The receiver is **unauthenticated** — GitHub calls it with no Atlas session —
//! so the HMAC is the only thing between a POST and card mutation. [`verify_signature`]
//! recomputes HMAC-SHA256 over the **raw** request body under the repo's stored
//! secret and compares in constant time. A missing or wrong signature is a 401 and
//! the body is never parsed, let alone acted on.
//!
//! The HMAC must run over the bytes *before* any JSON round-trip — re-serialising
//! would change them and break the signature — which is why the handler extracts
//! [`axum::body::Bytes`] and calls this, rather than taking a `Json<T>` extractor
//! that would make correct verification structurally impossible
//! (`docs/research/github-api.md` §7).

use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::error::{AppError, AppResult};

/// The header carrying the HMAC-SHA256 signature. The SHA-1 `X-Hub-Signature`
/// (no `-256`) is legacy and deliberately ignored.
pub const SIGNATURE_HEADER: &str = "x-hub-signature-256";

/// The header naming the event (`push`, `pull_request`, …).
pub const EVENT_HEADER: &str = "x-github-event";

/// The header carrying the delivery GUID — the natural idempotency key.
pub const DELIVERY_HEADER: &str = "x-github-delivery";

/// Verifies a GitHub webhook signature over the raw body, in constant time.
///
/// Returns `true` only when `header` is `sha256=<hex>` and the hex decodes to
/// exactly the HMAC-SHA256 of `raw_body` under `secret`. Every other case — no
/// `sha256=` prefix, non-hex, wrong length, wrong digest — returns `false`.
///
/// # Constant time
///
/// The comparison is [`subtle::ConstantTimeEq`] over the raw digest bytes, so the
/// time taken does not depend on *where* a forged signature first differs — closing
/// the timing side-channel GitHub's docs explicitly warn against
/// (`docs/research/github-api.md` §7). A length mismatch (the digest is always 32
/// bytes) short-circuits to `false`; the length of a signature is not secret.
#[must_use]
pub fn verify_signature(secret: &[u8], raw_body: &[u8], header: &str) -> bool {
    let Some(hex_sig) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex::decode(hex_sig) else {
        return false;
    };

    // HMAC accepts a key of any length, so `new_from_slice` cannot fail here.
    let mut mac = <Hmac<Sha256>>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(raw_body);
    let computed = mac.finalize().into_bytes();

    // `ct_eq` returns 0 (not in a data-dependent way) when the lengths differ, and
    // otherwise compares every byte regardless of the first mismatch.
    computed.ct_eq(expected.as_slice()).into()
}

// ---------------------------------------------------------------------------
// Event payloads — parsed only after the signature has been verified
// ---------------------------------------------------------------------------

/// The subset of a GitHub webhook delivery Atlas acts on.
///
/// Events Atlas does not handle parse to `None` from [`parse_event`] rather than an
/// error — a delivery for an event we did not subscribe to (or a new one GitHub
/// adds) is acknowledged and ignored, not failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookEvent {
    /// Commits were pushed to a branch.
    Push {
        /// The branch, with `refs/heads/` stripped.
        branch: String,
        /// The pushed commits (capped at 2048 by GitHub; a full push is truncated).
        commits: Vec<PushCommit>,
    },
    /// A pull request changed.
    PullRequest {
        /// `opened | closed | synchronize | …`.
        action: String,
        /// The head branch name.
        branch: String,
        /// Whether the PR is merged. The only signal that advances a card to Done.
        merged: bool,
        /// The PR number.
        number: i64,
        /// The PR title.
        title: String,
        /// The browser URL.
        html_url: String,
    },
    /// A check suite completed (the only `check_suite` action a repo hook receives).
    CheckSuite {
        /// The head commit SHA the suite ran against.
        head_sha: String,
        /// The head branch, if GitHub supplied one.
        head_branch: Option<String>,
        /// The suite conclusion (`success | failure | …`).
        conclusion: Option<String>,
    },
    /// A branch or tag was created.
    Create {
        /// `branch | tag`.
        ref_type: String,
        /// The created ref's short name.
        ref_name: String,
    },
    /// A branch or tag was deleted.
    Delete {
        /// `branch | tag`.
        ref_type: String,
        /// The deleted ref's short name.
        ref_name: String,
    },
}

/// A single commit in a `push` payload.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PushCommit {
    /// The commit SHA.
    pub id: String,
    /// The full commit message.
    pub message: String,
    /// The browser URL.
    #[serde(default)]
    pub url: String,
}

/// Strips a fully-qualified `refs/heads/` prefix, leaving the branch name.
#[must_use]
pub fn strip_branch_ref(git_ref: &str) -> &str {
    git_ref.strip_prefix("refs/heads/").unwrap_or(git_ref)
}

/// Parses a verified delivery into a [`WebhookEvent`], or `None` for an event Atlas
/// does not act on.
///
/// **Only call this after [`verify_signature`] has passed.** It deserialises the
/// body, which is exactly what an unverified delivery must never reach.
pub fn parse_event(event_name: &str, body: &[u8]) -> AppResult<Option<WebhookEvent>> {
    let json: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| AppError::BadRequest(format!("invalid webhook JSON: {e}")))?;

    Ok(match event_name {
        "push" => {
            #[derive(Deserialize)]
            struct Push {
                #[serde(default, rename = "ref")]
                git_ref: String,
                #[serde(default)]
                commits: Vec<PushCommit>,
            }
            let push: Push = serde_json::from_value(json).map_err(AppError::internal)?;
            Some(WebhookEvent::Push {
                branch: strip_branch_ref(&push.git_ref).to_owned(),
                commits: push.commits,
            })
        }
        "pull_request" => {
            #[derive(Deserialize)]
            struct Payload {
                #[serde(default)]
                action: String,
                pull_request: Pr,
            }
            #[derive(Deserialize)]
            struct Pr {
                #[serde(default)]
                number: i64,
                #[serde(default)]
                title: String,
                #[serde(default)]
                html_url: String,
                #[serde(default)]
                merged: bool,
                #[serde(default)]
                merged_at: Option<String>,
                head: Head,
            }
            #[derive(Deserialize)]
            struct Head {
                #[serde(default, rename = "ref")]
                branch: String,
            }
            let payload: Payload = serde_json::from_value(json).map_err(AppError::internal)?;
            Some(WebhookEvent::PullRequest {
                action: payload.action,
                branch: payload.pull_request.head.branch,
                merged: payload.pull_request.merged || payload.pull_request.merged_at.is_some(),
                number: payload.pull_request.number,
                title: payload.pull_request.title,
                html_url: payload.pull_request.html_url,
            })
        }
        "check_suite" => {
            #[derive(Deserialize)]
            struct Payload {
                check_suite: Suite,
            }
            #[derive(Deserialize)]
            struct Suite {
                #[serde(default)]
                head_sha: String,
                #[serde(default)]
                head_branch: Option<String>,
                #[serde(default)]
                conclusion: Option<String>,
            }
            let payload: Payload = serde_json::from_value(json).map_err(AppError::internal)?;
            Some(WebhookEvent::CheckSuite {
                head_sha: payload.check_suite.head_sha,
                head_branch: payload.check_suite.head_branch,
                conclusion: payload.check_suite.conclusion,
            })
        }
        "create" | "delete" => {
            #[derive(Deserialize)]
            struct Payload {
                #[serde(default, rename = "ref")]
                ref_name: String,
                #[serde(default)]
                ref_type: String,
            }
            let payload: Payload = serde_json::from_value(json).map_err(AppError::internal)?;
            if event_name == "create" {
                Some(WebhookEvent::Create {
                    ref_type: payload.ref_type,
                    ref_name: payload.ref_name,
                })
            } else {
                Some(WebhookEvent::Delete {
                    ref_type: payload.ref_type,
                    ref_name: payload.ref_name,
                })
            }
        }
        // Anything else — `ping`, `status`, an event we did not subscribe to — is
        // acknowledged and ignored rather than failed.
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference HMAC-SHA256 of a body under a key, as `sha256=<hex>`.
    fn sign(secret: &[u8], body: &[u8]) -> String {
        let mut mac = <Hmac<Sha256>>::new_from_slice(secret).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn a_valid_signature_verifies() {
        let secret = b"a-per-repo-webhook-secret";
        let body = br#"{"zen":"Keep it logically awesome."}"#;
        assert!(verify_signature(secret, body, &sign(secret, body)));
    }

    #[test]
    fn a_wrong_signature_does_not_verify() {
        let secret = b"a-per-repo-webhook-secret";
        let body = br#"{"action":"closed"}"#;
        // Right shape, wrong digest (signed under a different key).
        let forged = sign(b"attacker-guess", body);
        assert!(!verify_signature(secret, body, &forged));
        // A tampered body no longer matches its own signature.
        let sig = sign(secret, body);
        assert!(!verify_signature(secret, br#"{"action":"opened"}"#, &sig));
    }

    #[test]
    fn a_missing_prefix_or_non_hex_does_not_verify() {
        let secret = b"s";
        let body = b"{}";
        // No `sha256=` prefix.
        assert!(!verify_signature(secret, body, &hex::encode([0u8; 32])));
        // Prefixed but not hex.
        assert!(!verify_signature(secret, body, "sha256=zzzz"));
        // Prefixed, hex, but the wrong length for a SHA-256 digest.
        assert!(!verify_signature(secret, body, "sha256=abcd"));
        // Empty.
        assert!(!verify_signature(secret, body, ""));
    }

    #[test]
    fn the_branch_ref_prefix_is_stripped() {
        assert_eq!(
            strip_branch_ref("refs/heads/feature/ATLAS-1-x"),
            "feature/ATLAS-1-x"
        );
        assert_eq!(strip_branch_ref("main"), "main");
    }

    #[test]
    fn a_push_event_parses_its_branch_and_commits() {
        let body = br#"{
            "ref": "refs/heads/feature/ATLAS-9-x",
            "commits": [
                {"id": "abc", "message": "ATLAS-9 #done", "url": "https://x/abc"}
            ]
        }"#;
        let event = parse_event("push", body).unwrap().unwrap();
        assert_eq!(
            event,
            WebhookEvent::Push {
                branch: "feature/ATLAS-9-x".to_owned(),
                commits: vec![PushCommit {
                    id: "abc".to_owned(),
                    message: "ATLAS-9 #done".to_owned(),
                    url: "https://x/abc".to_owned(),
                }],
            }
        );
    }

    #[test]
    fn a_merged_pull_request_parses_as_merged() {
        let body = br#"{
            "action": "closed",
            "pull_request": {
                "number": 7, "title": "Add login", "html_url": "https://x/7",
                "merged": true, "head": {"ref": "feature/ATLAS-42-add-login"}
            }
        }"#;
        let event = parse_event("pull_request", body).unwrap().unwrap();
        let WebhookEvent::PullRequest {
            merged,
            branch,
            action,
            number,
            ..
        } = event
        else {
            panic!("expected a pull_request event");
        };
        assert!(merged);
        assert_eq!(action, "closed");
        assert_eq!(number, 7);
        assert_eq!(branch, "feature/ATLAS-42-add-login");
    }

    #[test]
    fn a_closed_unmerged_pull_request_is_not_merged() {
        let body = br#"{
            "action": "closed",
            "pull_request": {
                "number": 8, "title": "Abandoned", "html_url": "https://x/8",
                "merged": false, "merged_at": null, "head": {"ref": "feature/ATLAS-43-x"}
            }
        }"#;
        let WebhookEvent::PullRequest { merged, .. } =
            parse_event("pull_request", body).unwrap().unwrap()
        else {
            panic!("expected a pull_request event");
        };
        assert!(!merged, "a closed-without-merge PR must not read as merged");
    }

    #[test]
    fn an_unhandled_event_parses_to_none() {
        assert_eq!(parse_event("ping", b"{}").unwrap(), None);
        assert_eq!(parse_event("status", b"{}").unwrap(), None);
    }
}
