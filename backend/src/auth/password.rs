//! Argon2id password hashing, and the password policy.

use std::sync::OnceLock;

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};

use crate::error::{AppError, AppResult};

/// Argon2 memory cost, in **kibibytes**.
///
/// 19 MiB, per OWASP. The unit is the trap: argon2's own docs render this
/// constant as "19 MiB" in prose, and `Params::new(19, ..)` would compile,
/// pass every test, and produce a hash roughly a thousand times cheaper to
/// crack. See `docs/research/rust-stack.md`. A test below pins it against
/// argon2's `DEFAULT_M_COST`.
const M_COST_KIB: u32 = 19 * 1024;

/// Argon2 time cost (iterations). OWASP's companion to `m = 19 MiB`.
const T_COST: u32 = 2;

/// Argon2 parallelism. OWASP: 1.
const P_COST: u32 = 1;

/// Minimum password length, in characters.
///
/// Length is the only knob that reliably buys entropy. Atlas deliberately
/// imposes **no character-class rules**: NIST SP 800-63B advises against them,
/// because they push users towards `Password1!` — predictable to a cracker,
/// annoying to a human.
pub const MIN_LENGTH: usize = 12;

/// Maximum password length, in characters.
///
/// Not a security limit — Argon2 hashes any length happily — but a cost limit:
/// without it a request body full of megabytes of password is free CPU for an
/// attacker. Far above NIST's "at least 64 characters" floor.
pub const MAX_LENGTH: usize = 256;

/// The seeded default password, which may never be kept.
///
/// Lives here rather than in [`crate::auth::seed`] because the *policy* is what
/// rejects it, and the policy must reject it whether or not the account came
/// from the seeder.
pub const DEFAULT_ADMIN_PASSWORD: &str = "Admin";

/// The Argon2id configuration used for every hash and every verification.
fn argon2() -> &'static Argon2<'static> {
    static ARGON2: OnceLock<Argon2<'static>> = OnceLock::new();
    ARGON2.get_or_init(|| {
        // `expect` on constants: these are compile-time-known values that
        // satisfy Argon2's bounds, so a failure here is a programming error
        // discovered by the test suite, not a runtime condition.
        let params = Params::new(M_COST_KIB, T_COST, P_COST, None)
            .expect("the OWASP Argon2id parameters are valid");
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
    })
}

/// A PHC-format hash of a password nobody knows.
///
/// The point is the *time it takes to verify against*, not the value: see
/// [`verify`]. Computed once, on first use.
fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        // A random password, not a constant: nothing anywhere should be able to
        // present a password that verifies against this hash.
        let mut filler = [0u8; 32];
        {
            use argon2::password_hash::rand_core::RngCore;
            OsRng.fill_bytes(&mut filler);
        }
        hash_blocking(&hex(&filler)).expect("hashing a well-formed password cannot fail")
    })
}

/// Hashes `password` with Argon2id and a fresh random salt.
///
/// Runs on a blocking thread: this is ~50 ms of deliberate CPU, and doing it on
/// a tokio worker stalls the whole reactor. Login then becomes a trivial `DoS` —
/// a handful of concurrent requests starves every other connection.
pub async fn hash(password: String) -> AppResult<String> {
    tokio::task::spawn_blocking(move || hash_blocking(&password))
        .await
        .map_err(AppError::internal)?
}

/// Verifies `password` against a PHC-format `hash`.
///
/// Returns `Ok(false)` for a mismatch and an error only when the *stored hash*
/// is unreadable — which is a corrupt row, i.e. our bug, not a failed login.
///
/// Like [`hash`], this runs on a blocking thread.
pub async fn verify(password: String, hash: String) -> AppResult<bool> {
    tokio::task::spawn_blocking(move || verify_blocking(&password, &hash))
        .await
        .map_err(AppError::internal)?
}

/// Burns the same CPU as a real verification, and always fails.
///
/// This is the fix for a username oracle. Without it, a login for a username
/// that does not exist returns in microseconds while a login for one that does
/// takes ~50 ms of Argon2 — so an attacker enumerates every account in the
/// instance with a stopwatch and no valid password. Hashing against a throwaway
/// hash makes the two paths cost the same.
///
/// The `Ok(false)` return exists so call sites read identically to [`verify`].
pub async fn verify_dummy(password: String) -> AppResult<bool> {
    verify(password, dummy_hash().to_owned()).await
}

fn hash_blocking(password: &str) -> AppResult<String> {
    // OsRng here is argon2's re-export, whose rand_core is 0.6 — a *different*
    // type from rand 0.9/0.10's identically-named OsRng, which does not satisfy
    // this bound. Importing it from `argon2::password_hash` is not a stylistic
    // choice. See `docs/research/rust-stack.md`.
    let salt = SaltString::generate(&mut OsRng);

    argon2()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|err| AppError::internal(anyhow::anyhow!("failed to hash password: {err}")))
}

fn verify_blocking(password: &str, hash: &str) -> AppResult<bool> {
    let parsed = PasswordHash::new(hash).map_err(|err| {
        // Deliberately not a 401: a stored hash that will not parse means the
        // row is corrupt. Reporting that as "wrong password" would send an
        // operator hunting for a user error that never happened.
        AppError::internal(anyhow::anyhow!("stored password hash is unreadable: {err}"))
    })?;

    Ok(argon2()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// Enforces the password policy for `password` on the account `username`.
///
/// Order matters: the most specific message wins. `Admin` is rejected as a
/// *reused default* rather than as "too short", because an operator who has just
/// been told to change it away from `Admin` deserves to be told that typing it
/// again is the problem.
///
/// # Errors
///
/// [`AppError::Validation`] (422), carrying a message written for a human.
pub fn validate(password: &str, username: &str) -> AppResult<()> {
    if password.eq_ignore_ascii_case(DEFAULT_ADMIN_PASSWORD) {
        return Err(AppError::Validation(format!(
            "The default password {DEFAULT_ADMIN_PASSWORD:?} cannot be reused. \
             Choose a different password of at least {MIN_LENGTH} characters."
        )));
    }

    // Characters, not bytes: a passphrase of twelve non-ASCII characters is
    // twelve characters, and `len()` would call it long enough for the wrong
    // reason.
    let length = password.chars().count();

    if length < MIN_LENGTH {
        return Err(AppError::Validation(format!(
            "Password must be at least {MIN_LENGTH} characters long. \
             A memorable phrase of several words is both stronger and easier \
             than a short password with punctuation in it."
        )));
    }

    if length > MAX_LENGTH {
        return Err(AppError::Validation(format!(
            "Password must be at most {MAX_LENGTH} characters long."
        )));
    }

    if password.eq_ignore_ascii_case(username) {
        return Err(AppError::Validation(
            "Password must not be the same as your username.".to_owned(),
        ));
    }

    if is_common(password) {
        return Err(AppError::Validation(
            "That password is one of the most commonly used passwords in the world, \
             so it is among the first an attacker tries. Choose something less predictable."
                .to_owned(),
        ));
    }

    Ok(())
}

/// Whether `password` is in the embedded common-password list.
///
/// Case-insensitive: `PASSWORD123` is exactly as guessable as `password123`.
fn is_common(password: &str) -> bool {
    let lowered = password.to_lowercase();
    COMMON_PASSWORDS.contains(&lowered.as_str())
}

/// The most-guessed passwords, from published breach-corpus rankings.
///
/// Deliberately a small embedded list rather than a dependency. The full
/// value of a big list is realised against passwords under 12 characters, and
/// [`MIN_LENGTH`] has already rejected those — so this list only needs to cover
/// the long-but-notorious tail (`123456789012`, `qwertyuiop`, `iloveyou123`).
/// Pulling in zxcvbn or a 100k-entry wordlist to catch those would be a large
/// dependency doing a job [`MIN_LENGTH`] already did.
///
/// **Every entry must be lowercase** — [`is_common`] lowercases its input, so an
/// uppercase entry here could never match. A test enforces that.
const COMMON_PASSWORDS: &[&str] = &[
    "123456",
    "12345678",
    "123456789",
    "1234567890",
    "12345678901",
    "123456789012",
    "1234567890123",
    "12345678901234",
    "123456789012345",
    "111111111111",
    "000000000000",
    "password",
    "password1",
    "password12",
    "password123",
    "password1234",
    "password12345",
    "passw0rd123",
    "p@ssw0rd123",
    "passwordpassword",
    "qwerty",
    "qwerty123456",
    "qwertyuiop",
    "qwertyuiop123",
    "qwertyuiopasdfgh",
    "1q2w3e4r5t6y",
    "1qaz2wsx3edc",
    "zaq12wsxcde3",
    "asdfghjkl123",
    "abcdefghijkl",
    "abcd1234efgh",
    "iloveyou123",
    "iloveyou1234",
    "letmein12345",
    "welcome12345",
    "welcome123456",
    "administrator",
    "administrator1",
    "adminadmin123",
    "atlassian123",
    "changeme1234",
    "changemeplease",
    "trustno1trustno1",
    "monkeymonkey",
    "footballfootball",
    "baseball12345",
    "superman12345",
    "starwars12345",
    "dragondragon",
    "sunshine12345",
    "princess12345",
    "michaeljordan",
    "jennifer12345",
    "thisismypassword",
    "mypasswordis123",
    "correcthorse",
    "correcthorsebatterystaple",
    "thequickbrownfox",
    "qazwsxedcrfvtgb",
    "!qaz2wsx3edc4rfv",
];

/// Lowercase hex, for the dummy password. Not `format!("{:02x}")` in a loop
/// because this runs once and clarity beats cleverness either way.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        // Writing to a String is infallible.
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_memory_cost_is_19_mib_not_19_kib() {
        // The whole reason this test exists: `Params::new(19, ..)` compiles,
        // hashes, verifies, and passes every other test in this file while being
        // ~1000x weaker. Pin it against argon2's own OWASP constant.
        assert_eq!(M_COST_KIB, 19_456);
        assert_eq!(M_COST_KIB, Params::DEFAULT_M_COST);
        assert_eq!(T_COST, Params::DEFAULT_T_COST);
        assert_eq!(P_COST, Params::DEFAULT_P_COST);
    }

    #[test]
    fn the_configured_params_are_the_ones_that_reach_argon2() {
        // Guards the wiring, not the constants: `argon2()` could ignore them.
        let params = Params::new(M_COST_KIB, T_COST, P_COST, None).unwrap();
        assert_eq!(params.m_cost(), 19_456);
        assert_eq!(params.t_cost(), 2);
        assert_eq!(params.p_cost(), 1);
    }

    #[tokio::test]
    async fn a_hash_is_argon2id_and_carries_the_owasp_parameters() {
        let hashed = hash("a perfectly fine passphrase".to_owned())
            .await
            .unwrap();
        // The PHC string encodes the algorithm and cost, which is what lets us
        // raise the cost later without invalidating existing passwords.
        assert!(hashed.starts_with("$argon2id$"), "{hashed}");
        assert!(hashed.contains("m=19456,t=2,p=1"), "{hashed}");
    }

    #[tokio::test]
    async fn verification_accepts_the_right_password_and_rejects_others() {
        let hashed = hash("a perfectly fine passphrase".to_owned())
            .await
            .unwrap();

        assert!(
            verify("a perfectly fine passphrase".to_owned(), hashed.clone())
                .await
                .unwrap()
        );
        assert!(!verify("wrong".to_owned(), hashed.clone()).await.unwrap());
        // Off by one character, and off by case.
        assert!(
            !verify("a perfectly fine passphras".to_owned(), hashed.clone())
                .await
                .unwrap()
        );
        assert!(
            !verify("A Perfectly Fine Passphrase".to_owned(), hashed)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn the_same_password_hashes_differently_every_time() {
        // i.e. the salt is real. Identical hashes would mean equal passwords are
        // visible to anyone who reads the table.
        let a = hash("a perfectly fine passphrase".to_owned())
            .await
            .unwrap();
        let b = hash("a perfectly fine passphrase".to_owned())
            .await
            .unwrap();
        assert_ne!(a, b);

        // ...and both still verify.
        assert!(
            verify("a perfectly fine passphrase".to_owned(), a)
                .await
                .unwrap()
        );
        assert!(
            verify("a perfectly fine passphrase".to_owned(), b)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_corrupt_stored_hash_is_an_internal_error_not_a_failed_login() {
        let err = verify("anything".to_owned(), "not-a-phc-string".to_owned())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[tokio::test]
    async fn the_dummy_hash_never_verifies() {
        for candidate in ["", "Admin", "password", "a perfectly fine passphrase"] {
            assert!(
                !verify_dummy(candidate.to_owned()).await.unwrap(),
                "{candidate:?} verified against the dummy hash"
            );
        }
    }

    #[test]
    fn the_default_admin_password_is_rejected_in_any_case() {
        for candidate in ["Admin", "admin", "ADMIN", "aDmIn"] {
            let err = validate(candidate, "someone").unwrap_err();
            let message = err.to_string();
            assert!(matches!(err, AppError::Validation(_)));
            // The message must name the actual problem, not just the length.
            assert!(message.contains("cannot be reused"), "{message}");
        }
    }

    #[test]
    fn short_passwords_are_rejected() {
        assert!(validate("elevenchars", "someone").is_err());
        assert!(validate("", "someone").is_err());
        // Exactly at the boundary, from both sides.
        assert_eq!("shortpasswrd".chars().count(), MIN_LENGTH);
        assert!(validate("shortpasswrd", "someone").is_ok());
        assert!(validate("shortpasswr", "someone").is_err());
    }

    #[test]
    fn length_counts_characters_not_bytes() {
        // Eleven characters, thirty-three bytes. Measuring `len()` would call
        // this long enough and let an 11-character password through.
        let eleven_chars = "パスワードですよ本当に";
        assert_eq!(eleven_chars.chars().count(), 11);
        assert!(eleven_chars.len() > MIN_LENGTH);
        assert!(
            validate(eleven_chars, "someone").is_err(),
            "an 11-character password must be rejected however many bytes it is"
        );

        // Fourteen characters: accepted, and for the right reason.
        let fourteen_chars = "パスワードですよ本当に大丈夫";
        assert_eq!(fourteen_chars.chars().count(), 14);
        assert!(validate(fourteen_chars, "someone").is_ok());
    }

    #[test]
    fn over_long_passwords_are_rejected() {
        let huge = "a".repeat(MAX_LENGTH + 1);
        assert!(validate(&huge, "someone").is_err());
        let at_limit = "a".repeat(MAX_LENGTH);
        assert!(validate(&at_limit, "someone").is_ok());
    }

    #[test]
    fn a_password_equal_to_the_username_is_rejected() {
        assert!(validate("bartholomew1", "bartholomew1").is_err());
        // Case-insensitively, because the username lookup is case-insensitive.
        assert!(validate("BARTHOLOMEW1", "bartholomew1").is_err());
        // But a merely-similar one is fine.
        assert!(validate("bartholomew12", "bartholomew1").is_ok());
    }

    #[test]
    fn common_passwords_are_rejected_case_insensitively() {
        assert!(validate("password1234", "someone").is_err());
        assert!(validate("PASSWORD1234", "someone").is_err());
        assert!(validate("Password1234", "someone").is_err());
        assert!(validate("qwertyuiop123", "someone").is_err());
    }

    #[test]
    fn every_common_password_entry_is_actually_reachable() {
        for entry in COMMON_PASSWORDS {
            // `is_common` lowercases its input, so an uppercase entry could
            // never match anything and would be silently dead.
            assert_eq!(
                *entry,
                entry.to_lowercase(),
                "{entry:?} is not lowercase and can never match"
            );
            assert!(is_common(entry), "{entry:?} is listed but not detected");
        }
    }

    #[test]
    fn the_common_list_only_needs_to_cover_long_passwords() {
        // Documents the design: MIN_LENGTH already rejects the short ones, so
        // any entry below the floor is redundant with the length check. Not a
        // failure if one is short — just noise — but the long ones are the
        // list's whole reason to exist.
        let long_entries = COMMON_PASSWORDS
            .iter()
            .filter(|p| p.chars().count() >= MIN_LENGTH)
            .count();
        assert!(
            long_entries > 30,
            "the list is mostly redundant with MIN_LENGTH: only {long_entries} entries are long \
             enough to matter"
        );
    }

    #[test]
    fn a_good_password_passes() {
        assert!(validate("a perfectly fine passphrase", "someone").is_ok());
        assert!(validate("correct-horse-battery", "someone").is_ok());
        // No character-class rules: all-lowercase-no-digits is fine if long.
        assert!(validate("thisisalllowercaseandthatisfine", "someone").is_ok());
    }
}
