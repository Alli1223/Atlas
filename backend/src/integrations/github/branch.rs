//! Turning a card into a git branch name, and doing it safely.
//!
//! The scheme is `{type}/{key}-{slug}` — e.g. `feature/ATLAS-42-add-login`. The
//! branch *type* prefix is configurable (`feature`, `bugfix`, …); the card key is
//! preserved verbatim because it is the token webhook handlers and smart commits
//! match on ([`crate::integrations::github::smart_commit`]); the slug is derived
//! from the summary and sanitised hard.
//!
//! # Why the sanitising is not optional
//!
//! A git ref name is fed straight into `POST /git/refs` and, later, into shell-free
//! but still-parsed contexts all over GitHub. `git check-ref-format` forbids a long
//! list of byte sequences — `..`, `~`, `^`, `:`, `?`, `*`, `[`, `\`, control
//! characters, spaces, a leading `.`, a trailing `.lock`, `@{`, and a trailing or
//! doubled `/`. A summary is free text a user typed, so it can contain every one of
//! those. [`slugify`] collapses the whole lot to `-`, which is the one separator
//! that is always legal, so a generated name is a valid ref by construction rather
//! than by hoping the summary was tame.

/// The longest a slug may grow, in characters.
///
/// The card *key* prefix is never truncated — it is what webhook routing matches
/// on — but the human-readable tail is bounded so a paragraph pasted into a summary
/// does not become a 400-character ref.
const MAX_SLUG_LEN: usize = 50;

/// The default branch-type prefix, used when the caller supplies none.
pub const DEFAULT_BRANCH_TYPE: &str = "feature";

/// Lower-cases `input` and collapses every run of non-`[a-z0-9]` to a single `-`,
/// then trims leading and trailing `-` and truncates to [`MAX_SLUG_LEN`].
///
/// The result is either empty or a string of `[a-z0-9]` groups joined by single
/// `-`, which contains none of the byte sequences `git check-ref-format` rejects.
#[must_use]
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_dash = false;

    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            // Any separator — a space, punctuation, an emoji — becomes a single
            // dash, but only once a real character has followed it, so runs do
            // not produce `--` and a leading separator produces nothing.
            let dash = usize::from(pending_dash && !out.is_empty());
            // The dash and the character are appended together, so the budget
            // check must count both: checking only *after* appending let a name
            // ending `…-x` land one char over MAX_SLUG_LEN. `out` is pure ASCII,
            // so its byte length is its character count.
            if out.len() + dash + 1 > MAX_SLUG_LEN {
                break;
            }
            if dash == 1 {
                out.push('-');
            }
            pending_dash = false;
            out.extend(ch.to_lowercase());
        } else {
            pending_dash = true;
        }
    }

    out
}

/// Builds a branch name for a card.
///
/// `branch_type` is sanitised the same way as the slug (an empty or all-separator
/// type falls back to [`DEFAULT_BRANCH_TYPE`]); `key` is the card key, used as-is;
/// `summary` becomes the slug. When the summary slugifies to nothing (a summary of
/// only punctuation), the name is just `{type}/{key}` — still a valid, unique ref.
#[must_use]
pub fn branch_name(branch_type: &str, key: &str, summary: &str) -> String {
    let branch_type = {
        let slug = slugify(branch_type);
        if slug.is_empty() {
            DEFAULT_BRANCH_TYPE.to_owned()
        } else {
            slug
        }
    };

    let slug = slugify(summary);
    if slug.is_empty() {
        format!("{branch_type}/{key}")
    } else {
        format!("{branch_type}/{key}-{slug}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_documented_example_is_produced_exactly() {
        assert_eq!(
            branch_name("feature", "ATLAS-42", "Add login"),
            "feature/ATLAS-42-add-login"
        );
    }

    #[test]
    fn spaces_and_punctuation_collapse_to_single_dashes() {
        // The whole point of the sanitiser: none of this is a legal ref byte, and
        // all of it becomes a single `-`, never `--`, never a leading/trailing one.
        assert_eq!(
            branch_name("feature", "ATLAS-7", "  Fix the!! broken   login/page  "),
            "feature/ATLAS-7-fix-the-broken-login-page"
        );
    }

    #[test]
    fn the_forbidden_git_ref_sequences_do_not_survive() {
        let name = branch_name("bugfix", "ATLAS-1", "a..b~c^d:e?f*g[h\\i j.lock");
        // A valid ref contains none of these once slugified.
        for forbidden in ["..", "~", "^", ":", "?", "*", "[", "\\", " ", ".lock"] {
            assert!(
                !name.contains(forbidden),
                "{name:?} still contains the forbidden sequence {forbidden:?}"
            );
        }
        // ...and it did not collapse to nothing: the key survives.
        assert!(name.starts_with("bugfix/ATLAS-1"), "{name}");
    }

    #[test]
    fn the_slug_is_truncated_but_the_key_is_never() {
        let long = "word ".repeat(50);
        let name = branch_name("feature", "ATLAS-999", &long);
        assert!(name.starts_with("feature/ATLAS-999-"), "{name}");
        // The slug portion is bounded; the prefix `feature/ATLAS-999-` is not part
        // of that budget, so the whole name is a little longer than the cap.
        let slug = name.strip_prefix("feature/ATLAS-999-").unwrap();
        assert!(
            slug.chars().count() <= MAX_SLUG_LEN,
            "slug {slug:?} is {} chars",
            slug.chars().count()
        );
        assert!(
            !slug.ends_with('-'),
            "truncation left a trailing dash: {slug}"
        );
    }

    #[test]
    fn an_empty_or_symbol_only_summary_yields_type_slash_key() {
        assert_eq!(branch_name("feature", "ATLAS-3", ""), "feature/ATLAS-3");
        assert_eq!(
            branch_name("feature", "ATLAS-3", "!!! ???"),
            "feature/ATLAS-3"
        );
    }

    #[test]
    fn an_empty_branch_type_falls_back_to_the_default() {
        assert_eq!(branch_name("", "ATLAS-3", "hi"), "feature/ATLAS-3-hi");
        assert_eq!(branch_name("  /  ", "ATLAS-3", "hi"), "feature/ATLAS-3-hi");
    }

    #[test]
    fn slugify_is_idempotent_on_its_own_output() {
        let once = slugify("Hello, World! Foo");
        assert_eq!(slugify(&once), once);
        assert_eq!(once, "hello-world-foo");
    }
}
