import { describe, expect, it } from 'vitest'

import {
  assessPassword,
  characterCount,
  DEFAULT_ADMIN_PASSWORD,
  MAX_LENGTH,
  MIN_LENGTH,
  type PasswordRule,
} from './password'

/** Reads one rule out of an assessment, failing loudly if it is not there at all. */
function rule(password: string, id: PasswordRule['id'], options?: { username?: string; confirm?: string }) {
  const found = assessPassword(password, options).rules.find((r) => r.id === id)
  if (!found) throw new Error(`no rule ${id} in the assessment`)
  return found
}

describe('policy rules', () => {
  it('requires at least MIN_LENGTH characters', () => {
    expect(rule('a'.repeat(MIN_LENGTH - 1), 'length').satisfied).toBe(false)
    expect(rule('a'.repeat(MIN_LENGTH), 'length').satisfied).toBe(true)
  })

  it('rejects a password longer than MAX_LENGTH', () => {
    // The backend's upper bound exists because Argon2 hashes whatever it is handed; a
    // client that ignored it would let the user type a novel and then eat a 422.
    expect(rule('a'.repeat(MAX_LENGTH), 'length').satisfied).toBe(true)
    expect(rule('a'.repeat(MAX_LENGTH + 1), 'length').satisfied).toBe(false)
  })

  it('counts characters, not UTF-16 code units', () => {
    // 12 emoji: 24 code units, 12 characters. The backend counts with chars().count(), so
    // a `.length` check here would disagree with the server about this exact input.
    const emoji = '😀'.repeat(MIN_LENGTH)
    expect(emoji.length).toBe(MIN_LENGTH * 2)
    expect(characterCount(emoji)).toBe(MIN_LENGTH)
    expect(rule(emoji, 'length').satisfied).toBe(true)

    // ...and one short of the floor is still one short.
    expect(rule('😀'.repeat(MIN_LENGTH - 1), 'length').satisfied).toBe(false)
  })

  it('rejects the seeded default password whatever its case', () => {
    expect(rule(DEFAULT_ADMIN_PASSWORD, 'notDefault').satisfied).toBe(false)
    expect(rule('admin', 'notDefault').satisfied).toBe(false)
    expect(rule('ADMIN', 'notDefault').satisfied).toBe(false)
    // A password that merely contains it is fine — it is equality, not a substring ban.
    expect(rule('Administrator general', 'notDefault').satisfied).toBe(true)
  })

  it('rejects a password equal to the username, case-insensitively', () => {
    expect(rule('alastair', 'notUsername', { username: 'Alastair' }).satisfied).toBe(false)
    expect(rule('ALASTAIR', 'notUsername', { username: 'alastair' }).satisfied).toBe(false)
    expect(rule('alastair rayner rides', 'notUsername', { username: 'Alastair' }).satisfied).toBe(true)
  })

  it('does not fire the username rule when there is no username to compare against', () => {
    // An empty username must not make the empty password "equal" to it and light the rule
    // up green for the wrong reason.
    expect(rule('', 'notUsername', { username: '' }).satisfied).toBe(false)
    expect(rule('a long enough passphrase', 'notUsername', { username: '' }).satisfied).toBe(true)
  })

  it('only includes the matches rule when a confirmation is being collected', () => {
    expect(assessPassword('a long enough passphrase').rules.map((r) => r.id)).not.toContain('matches')
    expect(
      assessPassword('a long enough passphrase', { confirm: '' }).rules.map((r) => r.id),
    ).toContain('matches')
  })

  it('satisfies the matches rule only on an exact match', () => {
    expect(rule('correct horse battery', 'matches', { confirm: 'correct horse battery' }).satisfied).toBe(true)
    expect(rule('correct horse battery', 'matches', { confirm: 'correct horse batteries' }).satisfied).toBe(false)
    // Case-sensitive, unlike every other rule: this one compares two things the user typed.
    expect(rule('correct horse battery', 'matches', { confirm: 'Correct Horse Battery' }).satisfied).toBe(false)
  })

  it('shows nothing as satisfied for an empty password', () => {
    const { rules, isValid } = assessPassword('', { username: 'Admin', confirm: '' })
    expect(rules.every((r) => !r.satisfied)).toBe(true)
    expect(isValid).toBe(false)
  })
})

describe('isValid', () => {
  it('is true only when every policy rule passes', () => {
    expect(assessPassword('correct horse battery staple', { username: 'Admin' }).isValid).toBe(true)
    expect(assessPassword('short', { username: 'Admin' }).isValid).toBe(false)
    expect(assessPassword('Admin', { username: 'Admin' }).isValid).toBe(false)
  })

  it('ignores the confirmation field', () => {
    // The server never sees `confirm`, so it cannot be part of "would the server take this".
    // The form still blocks submit on it — that is the form's job, not the policy's.
    const assessment = assessPassword('correct horse battery staple', {
      username: 'Admin',
      confirm: 'something else entirely',
    })
    expect(assessment.isValid).toBe(true)
    expect(assessment.rules.find((r) => r.id === 'matches')?.satisfied).toBe(false)
  })
})

describe('strength meter', () => {
  const strength = (password: string, username = 'Admin') =>
    assessPassword(password, { username }).strength

  it('scores 0 for anything the server would reject', () => {
    expect(strength('').score).toBe(0)
    expect(strength('short').score).toBe(0)
    expect(strength('Admin').score).toBe(0)
    // Long, varied, and identical to the username: still 0, because it still fails. Calling
    // this "Strong" would be a lie the user only finds out about on submit.
    expect(strength('Xy9!Xy9!Xy9!Xy9!', 'Xy9!Xy9!Xy9!Xy9!').score).toBe(0)
  })

  it('rewards length above all else', () => {
    const twelve = strength('abcdefghijkm').score
    const twenty = strength('abcdefghijkmnopqrstu').score
    const thirty = strength('abcdefghijkmnopqrstuvwxyzabcd').score

    expect(twelve).toBeLessThan(twenty)
    expect(twenty).toBeLessThan(thirty)
  })

  it('rates a four-word passphrase as strong', () => {
    // The backend tells users to do exactly this, so the meter had better agree.
    expect(strength('correct horse battery staple').score).toBe(4)
  })

  it('gives a bonus for character variety', () => {
    const plain = strength('abcdefghijklmnop').score
    const varied = strength('abcdefgH1jklmn!p').score
    expect(varied).toBeGreaterThan(plain)
  })

  it('caps a repetitive password at the bottom of the scale', () => {
    // 24 characters, and a search space of one character plus a length. Length alone would
    // score this 3.
    expect(strength('a'.repeat(24)).score).toBe(1)
    // The same failure wearing a different hat.
    expect(strength('abababababababababab').score).toBe(1)
    expect(strength('123412341234123412341234').score).toBe(1)
  })

  it('holds back a password built from a handful of characters', () => {
    // Seven distinct characters over 28: better than pure repetition, not "Strong".
    expect(strength('abcdefgabcdefgabcdefgabcdefg').score).toBeLessThanOrEqual(2)
  })

  it('labels every score consistently', () => {
    expect(strength('').label).toBe('Too weak')
    expect(strength('a'.repeat(24)).label).toBe('Weak')
    expect(strength('correct horse battery staple').label).toBe('Strong')
  })

  it('never scores outside 0-4', () => {
    const samples = [
      '',
      'a',
      'Admin',
      'a'.repeat(MIN_LENGTH),
      'a'.repeat(400),
      'correct horse battery staple with a great many more words after it',
      '😀'.repeat(30),
      'Xx1!'.repeat(20),
    ]
    for (const sample of samples) {
      const { score } = strength(sample)
      expect(score, sample).toBeGreaterThanOrEqual(0)
      expect(score, sample).toBeLessThanOrEqual(4)
    }
  })
})
