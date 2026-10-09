// Never settles while being imported. The step-phase deadline is exercised by
// `ui-shot-hanging.steps.mjs`, which hangs when *run*; this one hangs when
// *loaded*, covering the separate module-load budget. It needs top-level await,
// so it cannot be the same file.
await new Promise(() => {});
