import { KeyboardEvent, ReactNode, useEffect, useRef } from "react";

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
///
/// `dismissible` is the single ownership switch for "can this sheet be
/// dismissed right now". It gates the backdrop tap here, and callers gate
/// their own Cancel button and the app's back handling with the same flag —
/// otherwise a sheet that disables Cancel while busy is still dismissible by
/// tapping the backdrop or swiping back, and the in-flight work it was
/// guarding is lost (issue #2024 rank 9).
export function Sheet({
  onClose,
  testId,
  label,
  dismissible = true,
  children,
}: {
  onClose: () => void;
  testId?: string;
  label?: string;
  dismissible?: boolean;
  children: ReactNode;
}) {
  const panelRef = useRef<HTMLDivElement>(null);
  const previouslyFocused = useRef<HTMLElement | null>(null);

  // Move focus into the sheet on open and restore it on close, so the
  // dialog is actually reachable by keyboard/screen reader instead of
  // leaving focus behind on the page it covers.
  useEffect(() => {
    previouslyFocused.current =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const panel = panelRef.current;
    if (panel) {
      const first = panel.querySelector<HTMLElement>(
        'input, textarea, select, button:not([disabled]), [tabindex]:not([tabindex="-1"])',
      );
      (first ?? panel).focus();
    }
    return () => {
      previouslyFocused.current?.focus?.();
    };
  }, []);

  // Contain Tab within the sheet. Without this, Tab walks off into the
  // obscured page behind the backdrop.
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "Tab") return;
    const panel = panelRef.current;
    if (!panel) return;
    const focusable = Array.from(
      panel.querySelectorAll<HTMLElement>(
        'input, textarea, select, button:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ),
    ).filter((el) => el.offsetParent !== null || el === document.activeElement);
    if (focusable.length === 0) {
      e.preventDefault();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    } else if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    }
  };

  return (
    <div className="sheet-wrap" data-testid={testId}>
      {/* An inert backdrop is not a dismiss affordance: it stays mounted so
          the dimming is unchanged, but it stops being an escape hatch while
          `dismissible` is false. */}
      <div
        className="sheet-backdrop"
        data-testid={testId ? `${testId}-backdrop` : undefined}
        onClick={dismissible ? onClose : undefined}
      />
      <div
        className="sheet"
        role="dialog"
        aria-modal="true"
        aria-label={label}
        tabIndex={-1}
        ref={panelRef}
        onKeyDown={onKeyDown}
      >
        {children}
      </div>
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
