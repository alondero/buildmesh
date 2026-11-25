export interface Win32InputKeyEvent {
  type: string;
  key: string;
  shiftKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

export interface DisposableLike {
  dispose(): void;
}

export type RegisterWin32CsiHandler = (
  id: { prefix?: string; final: string },
  callback: (params: (number | number[])[]) => boolean,
) => DisposableLike;

export interface Win32InputModeTracker {
  readonly enabled: boolean;
  dispose(): void;
}

export const WIN32_INPUT_MODE_PARAM = 9001;
export const WIN32_SHIFT_ENTER_KEY_DOWN = '\x1b[13;28;13;1;16;1_';

function hasWin32InputModeParam(params: (number | number[])[]): boolean {
  return params.some((param) =>
    Array.isArray(param) ? param.includes(WIN32_INPUT_MODE_PARAM) : param === WIN32_INPUT_MODE_PARAM,
  );
}

export function createWin32InputModeTracker(
  registerCsiHandler: RegisterWin32CsiHandler,
): Win32InputModeTracker {
  let enabled = false;
  const registrations = [
    registerCsiHandler({ prefix: '?', final: 'h' }, (params) => {
      if (hasWin32InputModeParam(params)) enabled = true;
      return false;
    }),
    registerCsiHandler({ prefix: '?', final: 'l' }, (params) => {
      if (hasWin32InputModeParam(params)) enabled = false;
      return false;
    }),
  ];
  let disposed = false;
  return {
    get enabled() {
      return enabled;
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      for (const registration of registrations) registration.dispose();
    },
  };
}

export function encodeWin32InputModeKey(
  event: Win32InputKeyEvent,
  enabled: boolean,
): string | null {
  if (!enabled || event.type !== 'keydown') return null;
  if (
    event.key === 'Enter'
    && event.shiftKey
    && !event.ctrlKey
    && !event.altKey
    && !event.metaKey
  ) {
    return WIN32_SHIFT_ENTER_KEY_DOWN;
  }
  return null;
}
