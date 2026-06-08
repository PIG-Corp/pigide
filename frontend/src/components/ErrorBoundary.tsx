// B-7.1: simple class-based React error boundary.
//
// Wrap any sub-tree that can throw during render (Markdown / force-graph /
// xterm dispose race) and a single bad child won't take down the whole
// shell. Each boundary stores its own error + reset signal so a panel
// can be retried independently of the rest of the UI.

import { Component, type ReactNode } from "react";

interface Props {
  /** What this subtree renders. */
  children: ReactNode;
  /** Optional human label shown in the fallback. */
  label?: string;
  /** Optional custom fallback. If omitted, a generic empty-state is used. */
  fallback?: (err: Error, reset: () => void) => ReactNode;
}

interface State {
  err: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { err: null };

  static getDerivedStateFromError(err: Error): State {
    return { err };
  }

  componentDidCatch(err: Error, info: { componentStack?: string }) {
    // Centralised logging — no toast here, the boundary might be inside
    // the orchestrator panel that just crashed.
    // eslint-disable-next-line no-console
    console.error(`[ErrorBoundary${this.props.label ? `: ${this.props.label}` : ""}]`, err, info.componentStack);
  }

  reset = () => {
    this.setState({ err: null });
  };

  render() {
    const { err } = this.state;
    if (!err) return this.props.children;
    if (this.props.fallback) return this.props.fallback(err, this.reset);
    return (
      <div className="error-boundary-fallback" role="alert">
        <div className="error-boundary-title">
          {this.props.label ? `${this.props.label} crashed` : "Something went wrong"}
        </div>
        <pre className="error-boundary-msg">{err.message}</pre>
        <button className="error-boundary-reset" onClick={this.reset}>
          Retry
        </button>
      </div>
    );
  }
}
