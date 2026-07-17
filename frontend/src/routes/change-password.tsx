import { zodResolver } from '@hookform/resolvers/zod'
import { createFileRoute, Navigate, useNavigate } from '@tanstack/react-router'
import { ShieldAlert } from 'lucide-react'
import { useForm, useWatch } from 'react-hook-form'
import { z } from 'zod'

import { Button, Input } from '@/components/ui'
import {
  assessPassword,
  authErrorMessage,
  AuthScreen,
  DEFAULT_ADMIN_PASSWORD,
  MAX_LENGTH,
  MIN_LENGTH,
  PasswordStrengthMeter,
  useChangePassword,
  useCurrentUser,
  useLogout,
} from '@/features/auth'
import { ICON } from '@/lib/icon'

import styles from './change-password.module.css'

export const Route = createFileRoute('/change-password')({
  component: ChangePasswordRoute,
})

/**
 * Form-level validation.
 *
 * Only the rules that stop a submit the server is certain to reject. The live checklist
 * under the field is where the user actually reads the policy; repeating it as field errors
 * would say the same thing twice in two different voices.
 *
 * The client is not the gate. `password::validate` is, and it also owns the
 * common-password list this schema knows nothing about — so a password that passes here can
 * still come back 422, and that message is shown verbatim in the banner.
 */
const changePasswordSchema = z
  .object({
    currentPassword: z.string().min(1, 'Enter your current password'),
    newPassword: z
      .string()
      .min(MIN_LENGTH, `Password must be at least ${MIN_LENGTH} characters long`)
      .max(MAX_LENGTH, `Password must be at most ${MAX_LENGTH} characters long`),
    confirmPassword: z.string().min(1, 'Confirm your new password'),
  })
  .refine((values) => values.newPassword === values.confirmPassword, {
    message: 'Both passwords must match',
    path: ['confirmPassword'],
  })
  .refine((values) => values.newPassword !== values.currentPassword, {
    message: 'The new password must be different from the current one',
    path: ['newPassword'],
  })

type ChangePasswordForm = z.infer<typeof changePasswordSchema>

/**
 * The explanation of why this screen is in the way.
 *
 * "You must change your password" with no reason reads as bureaucracy and gets clicked
 * through resentfully. The reason here is concrete and worth stating: the credentials are
 * public knowledge, printed in the server's own boot log.
 */
function ForcedResetNotice({ username }: { username: string }) {
  return (
    <div className={styles.why}>
      <span className={styles.whyIcon}>
        <ShieldAlert {...ICON} aria-hidden="true" />
      </span>
      <span className={styles.whyBody}>
        <span className={styles.whyTitle}>You are signing in with the default credentials</span>
        <span>
          Atlas created <strong>{username}</strong> with the password{' '}
          <strong>{DEFAULT_ADMIN_PASSWORD}</strong> so that you could get in for the first
          time. Anyone who can reach this server knows that password too, so nothing else
          will open until you replace it.
        </span>
      </span>
    </div>
  )
}

function ChangePasswordRoute() {
  const { user } = useCurrentUser()
  const navigate = useNavigate()
  const changePassword = useChangePassword()
  const logout = useLogout()

  const {
    register,
    handleSubmit,
    control,
    formState: { errors },
  } = useForm<ChangePasswordForm>({
    resolver: zodResolver(changePasswordSchema),
    defaultValues: { currentPassword: '', newPassword: '', confirmPassword: '' },
    // The rules light up live under the field, so errors that contradict them must clear
    // live too. The default ('onSubmit') would leave "must be at least 12 characters"
    // sitting beneath a checklist that has already gone green.
    mode: 'onChange',
  })

  // `useWatch`, not the `watch()` returned by useForm: `watch` is a function that closes
  // over mutable form state, so React Compiler cannot memoize anything that calls it and
  // silently skips optimising the whole component (eslint-plugin-react-hooks@7 reports the
  // bail-out, which is how this was caught). `useWatch` subscribes properly and re-renders
  // only this component.
  const newPassword = useWatch({ control, name: 'newPassword' })
  const confirmPassword = useWatch({ control, name: 'confirmPassword' })

  // AuthGate only renders this route for a signed-in user, so `user` is present by the time
  // this component exists. This covers the window after "Sign out instead" clears the
  // cache but before the navigation lands.
  if (user == null) {
    return <Navigate to="/login" replace />
  }

  const isForced = user.mustChangePassword

  const assessment = assessPassword(newPassword, {
    username: user.username,
    confirm: confirmPassword,
  })

  const onSubmit = handleSubmit((values) => {
    changePassword.mutate(
      { currentPassword: values.currentPassword, newPassword: values.newPassword },
      {
        onSuccess: () => {
          // The response cleared mustChangePassword and seeded the `/me` cache with it, so
          // the gate is already open by the time this runs.
          void navigate({ to: '/', replace: true })
        },
      },
    )
  })

  return (
    <AuthScreen
      title={isForced ? 'Choose a password' : 'Change your password'}
      {...(isForced
        ? {}
        : {
            lede: 'Changing your password signs you out of every other session, on every device.',
          })}
      {...(changePassword.isError ? { error: authErrorMessage(changePassword.error) } : {})}
    >
      {isForced && <ForcedResetNotice username={user.username} />}

      {/* noValidate: the browser's own bubbles would pre-empt the field errors below, which
          are the ones wired to aria-describedby. `void onSubmit(event)`: handleSubmit returns
          a promise, and a promise handed to a DOM event attribute turns any rejection into
          an unhandled one. */}
      <form className={styles.form} onSubmit={(event) => void onSubmit(event)} noValidate>
        <div className={styles.fields}>
          <Input
            label="Current password"
            type="password"
            autoComplete="current-password"
            autoFocus
            {...(errors.currentPassword?.message !== undefined
              ? { errorMessage: errors.currentPassword.message }
              : {})}
            {...register('currentPassword')}
          />
          <Input
            label="New password"
            type="password"
            autoComplete="new-password"
            {...(errors.newPassword?.message !== undefined
              ? { errorMessage: errors.newPassword.message }
              : {})}
            {...register('newPassword')}
          />
          <Input
            label="Confirm new password"
            type="password"
            autoComplete="new-password"
            {...(errors.confirmPassword?.message !== undefined
              ? { errorMessage: errors.confirmPassword.message }
              : {})}
            {...register('confirmPassword')}
          />
        </div>

        <PasswordStrengthMeter assessment={assessment} isEmpty={newPassword === ''} />

        <Button
          type="submit"
          appearance="primary"
          shouldFitContainer
          isLoading={changePassword.isPending}
        >
          {isForced ? 'Set password and continue' : 'Change password'}
        </Button>
      </form>

      {/* The only way off this screen other than through it. A user who cannot change their
          password must still be able to leave — which is why logout is one of the three
          routes the backend's forced-reset gate lets past. */}
      {isForced && (
        <div className={styles.signOut}>
          <Button
            appearance="subtle"
            isLoading={logout.isPending}
            onClick={() => {
              logout.mutate(undefined, {
                onSuccess: () => void navigate({ to: '/login', replace: true }),
              })
            }}
          >
            Sign out instead
          </Button>
        </div>
      )}
    </AuthScreen>
  )
}
