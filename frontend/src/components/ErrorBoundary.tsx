import { Component } from 'react'
import type { ErrorInfo, ReactNode } from 'react'

type ErrorBoundaryProps = {
  children: ReactNode
}

type ErrorBoundaryState = {
  error: Error | null
}

/**
 * Catches render/runtime errors from the routed tree and shows a token-styled
 * crash card instead of a blank white screen. Reload re-boots the app.
 */
export class ErrorBoundary extends Component<
  ErrorBoundaryProps,
  ErrorBoundaryState
> {
  state: ErrorBoundaryState = { error: null }

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error }
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error('Unhandled UI error:', error, info)
  }

  handleReload = () => {
    window.location.reload()
  }

  render() {
    if (this.state.error) {
      return (
        <div className="crash-card" role="alert">
          <span className="eyebrow">Something broke</span>
          <h2>The interface hit an unexpected error.</h2>
          <p>
            Your canonical library is safe — this only affected the on-screen
            view. Reloading usually clears it.
          </p>
          <p className="crash-card__detail">{this.state.error.message}</p>
          <div className="crash-card__actions">
            <button
              className="btn btn--primary"
              onClick={this.handleReload}
              type="button"
            >
              Reload app
            </button>
          </div>
        </div>
      )
    }

    return this.props.children
  }
}
