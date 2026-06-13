export const isMac = navigator.platform.toUpperCase().includes('MAC');

// Path comparison is case-insensitive on Windows (the OS file lookup is the
// source of truth) and case-sensitive on macOS/Linux. Used by path
// normalization in `lib/paths.ts` to decide whether to lowercase.
export const isWindows = navigator.platform.toLowerCase().startsWith('win');
