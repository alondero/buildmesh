/** Convert a desired visible dock width back to its flex-basis preference. */
export function preferredDockWidth(panel: HTMLElement | null, desired: number): number {
  const parent = panel?.parentElement;
  const workspace = parent?.querySelector<HTMLElement>(':scope > [data-testid="terminal-workspace"]');
  const other = parent && Array.from(parent.children).find((child): child is HTMLElement =>
    child instanceof HTMLElement && child !== panel && child.hasAttribute('data-workspace-dock'));
  if (!panel || !parent || !workspace || !other) return desired;
  const available = parent.clientWidth - parseFloat(getComputedStyle(workspace).minWidth);
  const otherMinimum = parseFloat(getComputedStyle(other).minWidth);
  const minimum = parseFloat(getComputedStyle(panel).minWidth);
  const otherPreferred = parseFloat(other.style.width);
  if (![available, otherMinimum, minimum, otherPreferred].every(Number.isFinite) || available <= otherMinimum) return desired;
  const target = Math.max(minimum, Math.min(desired, available - otherMinimum));
  if (Math.abs(target - panel.getBoundingClientRect().width) < 0.5) return parseFloat(panel.style.width);
  if (target + otherPreferred <= available) return target;
  return target * otherPreferred / (available - target);
}
