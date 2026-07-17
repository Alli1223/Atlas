import { zodResolver } from '@hookform/resolvers/zod'
import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { useForm } from 'react-hook-form'
import { z } from 'zod'

import { Button, Input } from '@/components/ui'
import {
  authErrorMessage,
  AuthScreen,
  NavigateToHref,
  safeRedirect,
  useCurrentUser,
  useLogin,
} from '@/features/auth'

import styles from './login.module.css'

/** `/login`'s search params. */
export interface LoginSearch {
  /** Where the user was headed before the guard bounced them here. */
  redirect?: string
}

/**
 * Hand-written rather than a zod schema: the router calls this on *every* navigation to
 * `/login`, including the ones the guard makes, and a schema that throws on a malformed
 * `redirect` would turn a junk URL into a crashed router rather than a login screen.
 * Dropping the param is the correct response to garbage in it.
 *
 * The value is not trusted here — see `safeRedirect`, which is what actually decides
 * whether it is safe to navigate to.
 */
function validateSearch(search: Record<string, unknown>): LoginSearch {
  // Conditional spread rather than `{ redirect: undefined }`: exactOptionalPropertyTypes
  // draws a real distinction between an absent key and an explicit undefined.
  return typeof search.redirect === 'string' && search.redirect !== ''
    ? { redirect: search.redirect }
    : {}
}

export const Route = createFileRoute('/login')({
  validateSearch,
  component: LoginRoute,
})

/**
 * Presence checks only.
 *
 * The password policy is deliberately NOT applied here. This form authenticates an
 * *existing* credential: an account created before the policy tightened would find itself
 * unable to type its own password into a field that rejects it before the server ever sees
 * it. Policy belongs on change-password, where a new password is being chosen.
 */
const loginSchema = z.object({
  username: z
    .string()
    .min(1, 'Enter your username')
    .refine((value) => value.trim().length > 0, 'Enter your username'),
  password: z.string().min(1, 'Enter your password'),
})

type LoginForm = z.infer<typeof loginSchema>

function LoginRoute() {
  const { redirect } = Route.useSearch()
  const navigate = useNavigate()
  const login = useLogin()
  const { user } = useCurrentUser()

  const target = safeRedirect(redirect)

  const {
    register,
    handleSubmit,
    formState: { errors },
  } = useForm<LoginForm>({
    resolver: zodResolver(loginSchema),
    defaultValues: { username: '', password: '' },
  })

  // Already signed in and past the gate — nothing to do here. (A user who still owes a
  // password change never reaches this component: AuthGate sends them on first.)
  //
  // A component rather than a `navigate()` call in this branch: navigating during render is
  // a side effect in render, which React Compiler's purity rules reject — rightly, since it
  // would fire again on every re-render before the navigation lands.
  if (user != null && !user.mustChangePassword) {
    return <NavigateToHref href={target} />
  }

  const onSubmit = handleSubmit((values) => {
    login.mutate(
      { username: values.username.trim(), password: values.password },
      {
        onSuccess: (signedIn) => {
          // A user who must reset lands on the reset screen, not on the page they asked
          // for — and the guard would bounce them there regardless. Doing it here as well
          // keeps the redirect off the history stack.
          if (signedIn.mustChangePassword) {
            void navigate({ to: '/change-password', replace: true })
            return
          }
          void navigate({ href: target, replace: true })
        },
      },
    )
  })

  return (
    <AuthScreen
      title="Log in to Atlas"
      lede="Enter your credentials to continue."
      {...(login.isError ? { error: authErrorMessage(login.error) } : {})}
    >
      {/* noValidate: the browser's own bubbles would pre-empt the field errors below, which
          are the ones wired to aria-describedby.

          `void onSubmit(event)`: react-hook-form's handleSubmit returns a promise, and
          handing a promise-returning function to a DOM event attribute means a rejection
          becomes an unhandled one. It cannot reject here — the mutation's error goes into
          `login.error` — so discarding it is right, but it has to be discarded deliberately. */}
      <form className={styles.form} onSubmit={(event) => void onSubmit(event)} noValidate>
        <div className={styles.fields}>
          <Input
            label="Username"
            autoComplete="username"
            // The first thing on a first run, and the only field anyone starts from.
            autoFocus
            {...(errors.username?.message !== undefined
              ? { errorMessage: errors.username.message }
              : {})}
            {...register('username')}
          />
          <Input
            label="Password"
            type="password"
            autoComplete="current-password"
            {...(errors.password?.message !== undefined
              ? { errorMessage: errors.password.message }
              : {})}
            {...register('password')}
          />
        </div>

        <Button type="submit" appearance="primary" shouldFitContainer isLoading={login.isPending}>
          Log in
        </Button>
      </form>
    </AuthScreen>
  )
}
