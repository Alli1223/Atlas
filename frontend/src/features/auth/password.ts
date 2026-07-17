/**
 * The password policy, mirrored from `crate::auth::password`.
 *
 * # The server is the gate; this is the feedback
 *
 * Nothing here decides whether a password is accepted — `password::validate` on the
 * backend does, and it also owns the common-password list this module deliberately does
 * not duplicate. What this module buys is *live* feedback: the rules light up as you type
 * instead of after a round-trip. Any rule the client misses is still caught by the server
 * and surfaced verbatim in the error banner, so the two disagreeing costs a round-trip,
 * never a weak password.
 *
 * # Why no zxcvbn
 *
 * TODO.md asks for a zxcvbn meter, and the backend's own policy notes explain why it is the
 * wrong tool here: zxcvbn's value is concentrated in short passwords, and `MIN_LENGTH` has
 * already rejected every one of those. It is a ~400kB dependency (bigger than the whole
 * current bundle) doing a job the length floor did, and it is not in the verified
 * dependency set. This is a small deterministic estimator instead — one that agrees with
 * the backend's stated reasoning that length dominates: "a memorable phrase of several
 * words is both stronger and easier than a short password with punctuation in it".
 */

/** Minimum length, in characters. `crate::auth::password::MIN_LENGTH`. */
export const MIN_LENGTH = 12

/** Maximum length, in characters. `crate::auth::password::MAX_LENGTH`. */
export const MAX_LENGTH = 256

/** The seeded default password, which may never be reused. `DEFAULT_ADMIN_PASSWORD`. */
export const DEFAULT_ADMIN_PASSWORD = 'Admin'

/** The seeded default username. `crate::auth::seed::DEFAULT_ADMIN_USERNAME`. */
export const DEFAULT_ADMIN_USERNAME = 'Admin'

/**
 * Counts characters, not UTF-16 code units.
 *
 * `'😀'.length` is 2 and `[...'😀'].length` is 1. The backend counts with
 * `chars().count()`, so a client using `.length` would call an 8-emoji password long
 * enough and then watch the server reject it — the two must count the same things.
 */
export function characterCount(password: string): number {
  return [...password].length
}

/** A single policy rule, and whether the current input satisfies it. */
export interface PasswordRule {
  /** Stable identifier — the test hook and the React key. Never shown. */
  id: 'length' | 'notDefault' | 'notUsername' | 'matches'
  /** What the user reads. */
  label: string
  satisfied: boolean
}

/** How strong a password looks. `score` drives the meter; `label` is shown beside it. */
export interface PasswordStrength {
  /** 0 (unusable) to 4 (strong). */
  score: 0 | 1 | 2 | 3 | 4
  label: 'Too weak' | 'Weak' | 'Fair' | 'Good' | 'Strong'
}

export interface PasswordAssessment {
  rules: PasswordRule[]
  strength: PasswordStrength
  /** Whether every *policy* rule passes — i.e. whether the server should accept it. */
  isValid: boolean
}

const STRENGTH_LABELS = ['Too weak', 'Weak', 'Fair', 'Good', 'Strong'] as const

export interface AssessOptions {
  /** The account's username. A password equal to it is rejected. */
  username?: string
  /** The confirmation field, when there is one. Omit to skip the `matches` rule. */
  confirm?: string
}

/**
 * Evaluates `password` against the policy and estimates its strength.
 *
 * Rule order matches the backend's, and for the same reason: the most specific message
 * wins. An operator who has just been told to change away from `Admin` should be told that
 * typing it again is the problem — not that it is "too short".
 */
export function assessPassword(password: string, options: AssessOptions = {}): PasswordAssessment {
  const { username, confirm } = options
  const length = characterCount(password)

  const isDefault = equalsIgnoreCase(password, DEFAULT_ADMIN_PASSWORD)
  const isUsername = username !== undefined && username !== '' && equalsIgnoreCase(password, username)

  const rules: PasswordRule[] = [
    {
      id: 'length',
      label: `At least ${MIN_LENGTH} characters`,
      satisfied: length >= MIN_LENGTH && length <= MAX_LENGTH,
    },
    {
      id: 'notDefault',
      label: `Not the default password (${DEFAULT_ADMIN_PASSWORD})`,
      satisfied: password.length > 0 && !isDefault,
    },
    {
      id: 'notUsername',
      label: 'Not your username',
      satisfied: password.length > 0 && !isUsername,
    },
  ]

  if (confirm !== undefined) {
    rules.push({
      id: 'matches',
      label: 'Both passwords match',
      satisfied: password.length > 0 && password === confirm,
    })
  }

  // `matches` is a form rule, not a policy rule: the server never sees the confirm field,
  // so it cannot be part of "would the server accept this".
  const isValid = rules.every((rule) => rule.id === 'matches' || rule.satisfied)

  return { rules, strength: strengthOf(password, isValid), isValid }
}

/**
 * Estimates strength on the 0–4 scale the meter renders.
 *
 * A password that breaks a rule scores 0 regardless of how it looks: the server will
 * refuse it, so calling a 40-character password "Strong" while it is also the username
 * would be a lie the user finds out about on submit.
 */
function strengthOf(password: string, isValid: boolean): PasswordStrength {
  if (password.length === 0 || !isValid) return { score: 0, label: STRENGTH_LABELS[0] }

  const length = characterCount(password)

  // Length is the dominant term, matching the backend's own advice.
  let score = 1
  if (length >= 16) score += 1
  if (length >= 20) score += 1
  if (length >= 28) score += 1

  // One bonus point for real variety: several character classes, or a multi-word
  // passphrase. Either is evidence of a larger search space than the length alone implies.
  if (characterClasses(password) >= 3 || wordCount(password) >= 4) {
    score += 1
  }

  // ...and a hard cap for repetition. "aaaaaaaaaaaaaaaaaaaaaaaa" is 24 characters and would
  // otherwise score 3; its actual search space is one character and a length. Unique
  // characters — not a regex for runs — because it also catches "abababab..." and
  // "123123123123", which are the same failure wearing a different hat.
  const unique = new Set([...password.toLowerCase()]).size
  if (unique <= 4) return { score: 1, label: STRENGTH_LABELS[1] }
  if (unique <= 7) score = Math.min(score, 2)

  const clamped = Math.min(score, 4) as 0 | 1 | 2 | 3 | 4
  return { score: clamped, label: STRENGTH_LABELS[clamped] }
}

/** How many of lower / upper / digit / symbol the password draws on. */
function characterClasses(password: string): number {
  const tests = [/\p{Ll}/u, /\p{Lu}/u, /\p{Nd}/u, /[^\p{L}\p{Nd}]/u]
  return tests.filter((test) => test.test(password)).length
}

/** How many whitespace-separated words the password has. */
function wordCount(password: string): number {
  return password.trim().split(/\s+/).filter(Boolean).length
}

function equalsIgnoreCase(a: string, b: string): boolean {
  // The backend compares with `eq_ignore_ascii_case`, which folds A-Z only. `toLowerCase`
  // folds more than that, so the client can only ever be *stricter* — it will never accept
  // something the server then rejects, which is the direction that matters.
  return a.toLowerCase() === b.toLowerCase()
}
