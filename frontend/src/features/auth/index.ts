export {
  AuthGate,
  CHANGE_PASSWORD_ROUTE,
  LOGIN_ROUTE,
  NavigateToHref,
  safeRedirect,
} from './AuthGate'
export { AuthScreen } from './AuthScreen'
export type { AuthScreenProps } from './AuthScreen'

export {
  changePassword,
  fetchMe,
  fetchSessions,
  login,
  logout,
  revokeSession,
} from './api'
export type { ChangePasswordInput, LoginInput, Role, Session, User } from './api'

export {
  assessPassword,
  characterCount,
  DEFAULT_ADMIN_PASSWORD,
  DEFAULT_ADMIN_USERNAME,
  MAX_LENGTH,
  MIN_LENGTH,
} from './password'
export type { AssessOptions, PasswordAssessment, PasswordRule, PasswordStrength } from './password'

export { PasswordStrength as PasswordStrengthMeter } from './PasswordStrength'
export type { PasswordStrengthProps } from './PasswordStrength'

export {
  authErrorMessage,
  isLockedOut,
  isPasswordChangeRequired,
  isUnauthorized,
  PROBLEM_TYPE,
} from './problem'

export {
  authKeys,
  meQueryOptions,
  useChangePassword,
  useCurrentUser,
  useLogin,
  useLogout,
  useSessions,
} from './queries'
