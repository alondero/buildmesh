import { ReactNode } from "react";

// Shared mobile chrome. Visual styling lives in styles.css so pressed
// states (:active) work — inline styles can't express pseudo-classes.

export function AppBar({
  onBack,
  backTestId,
  title,
  subtitle,
  children,
}: {
  onBack?: () => void;
  backTestId?: string;
  title: ReactNode;
  subtitle?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div className="appbar">
      {onBack && (
        <button
          className="icon-btn"
          onClick={onBack}
          aria-label="Back"
          data-testid={backTestId}
        >
          ←
        </button>
      )}
      <div style={{ flex: 1, minWidth: 0 }}>
        <div className="appbar-title">{title}</div>
        {subtitle != null && <div className="appbar-sub">{subtitle}</div>}
      </div>
      {children}
    </div>
  );
}

/// Bottom sheet anchored inside #root (not the layout viewport) so it stays
/// visible when the soft keyboard shrinks --app-height.
export function Sheet({
  onClose,
  testId,
  children,
}: {
  onClose: () => void;
  testId?: string;
  children: ReactNode;
}) {
  return (
    <div className="sheet-wrap" data-testid={testId}>
      <div className="sheet-backdrop" onClick={onClose} />
      <div className="sheet">{children}</div>
    </div>
  );
}

export function PulseDots() {
  return (
    <span className="pulse-dots" aria-label="Loading">
      <span />
      <span />
      <span />
    </span>
  );
}

export function CenterNote({
  children,
  testId,
}: {
  children: ReactNode;
  testId?: string;
}) {
  return (
    <div
      data-testid={testId}
      style={{
        color: "var(--text-faint)",
        padding: 24,
        textAlign: "center",
        fontSize: 13,
      }}
    >
      {children}
    </div>
  );
}

export function ScreenLoading({
  testId,
  label,
}: {
  testId?: string;
  label?: string;
}) {
  return (
    <div
      data-testid={testId}
      style={{
        flex: 1,
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 12,
        color: "var(--text-faint)",
        fontSize: 13,
      }}
    >
      <PulseDots />
      {label}
    </div>
  );
}
