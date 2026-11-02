import { Component, type ErrorInfo, type ReactNode } from 'react'

import { Banner, Button } from '@/components/ui'

interface State {
  error: Error | undefined
}

/**
 * Catches a render error below it and shows it in place, with a retry — one broken panel
 * must not blank the whole app (`TODO.md` Phase 19). `resetKey` (the route path) clears it
 * when the user navigates away.
 */
export class ErrorBoundary extends Component<{ children: ReactNode; resetKey?: string }, State> {
  override state: State = { error: undefined }

  static getDerivedStateFromError(error: Error): State {
    return { error }
  }

  override componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error('Render error', error, info.componentStack)
  }

  override componentDidUpdate(previous: { resetKey?: string }): void {
    if (this.state.error && previous.resetKey !== this.props.resetKey) {
      this.setState({ error: undefined })
    }
  }

  override render() {
    if (this.state.error) {
      return (
        <div style={{ padding: 'var(--ds-space-300)' }}>
          <Banner
            appearance="error"
            actions={
              <Button appearance="default" size="compact" onClick={() => this.setState({ error: undefined })}>
                Try again
              </Button>
            }
          >
            Something went wrong showing this page: {this.state.error.message}
          </Banner>
        </div>
      )
    }
    return this.props.children
  }
}
