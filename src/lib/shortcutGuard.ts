export function createShortcutGuard(cooldownMs: number = 300) {
  let blocked = false;

  return (action: () => Promise<void>): void => {
    if (blocked) return;
    blocked = true;

    const start = Date.now();
    action().catch(() => {}).finally(() => {
      const elapsed = Date.now() - start;
      const remaining = cooldownMs - elapsed;
      if (remaining > 0) {
        setTimeout(() => { blocked = false; }, remaining);
      } else {
        blocked = false;
      }
    });
  };
}
