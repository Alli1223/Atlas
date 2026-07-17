//! Deserialisers the request bodies share.

use serde::{Deserialize, Deserializer};

/// Deserialises `Option<Option<T>>` so that `null` and absence stay different.
///
/// Without this the type is a lie. `#[serde(default)]` on a bare
/// `Option<Option<T>>` gives `None` for an absent field — correct — and *also*
/// `None` for an explicit `null`, because the outer `Option`'s own `null`
/// handling consumes it before the inner one is ever asked. Both cases collapse
/// to "leave it alone", and `{"assigneeId": null}` silently does nothing: the
/// user clicks "unassign", gets a 200, and the card is still assigned.
///
/// Deserialising the *inner* `Option<T>` and wrapping the result in `Some`
/// unconditionally is what fixes it. `#[serde(default)]` still supplies the
/// `None` for the absent case, because `deserialize_with` is not called at all
/// when the field is missing.
///
/// Phase 2 hit this and fixed it locally in `api::users`; Phase 3 needs it on
/// nine card fields, so it lives here now and `api::users` reads it from here
/// rather than keeping a second copy to drift from.
// clippy::option_option suggests "a custom enum if you need to distinguish all 3
// cases" — which is exactly the situation, and the enum would need a
// hand-written Deserialize to recover what serde already does for
// Option<Option<T>> with the two lines below. `docs/research/rust-stack.md` §8
// names this as the PATCH pattern for the project.
#[allow(clippy::option_option)]
pub fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[allow(clippy::option_option)]
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Patch {
        #[serde(default, deserialize_with = "double_option")]
        assignee_id: Option<Option<String>>,
    }

    #[test]
    fn absent_null_and_a_value_are_three_different_things() {
        // The whole reason this function exists. A plain Option collapses the
        // first two, which makes "unassign" impossible to express.
        let absent: Patch = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.assignee_id, None, "absent means leave it alone");

        let null: Patch = serde_json::from_str(r#"{"assigneeId":null}"#).unwrap();
        assert_eq!(null.assignee_id, Some(None), "null means clear it");

        let set: Patch = serde_json::from_str(r#"{"assigneeId":"u1"}"#).unwrap();
        assert_eq!(set.assignee_id, Some(Some("u1".to_owned())));
    }
}
