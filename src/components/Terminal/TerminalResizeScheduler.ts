/**
 * Owns the resize-observation policy shared by agent and build/run terminals.
 *
 * A ResizeObserver can fire once per layout frame while a split-pane handle
 * moves. The scheduler keeps the terminal visually current with a trailing
 * quiet period, but caps the delay so a long drag is not frozen. The actual
 * fit runs on the next animation frame after either timer, keeping DOM
 * measurement aligned with the browser render loop.
 */
export const TERMINAL_RESIZE_QUIET_MS = 50;
export const TERMINAL_RESIZE_MAX_WAIT_MS = 100;

type FrameScheduler = (callback: () => void) => void;

export class TerminalResizeScheduler {
  private observer: ResizeObserver | null = null;
  private quietTimer: ReturnType<typeof setTimeout> | null = null;
  private maxWaitTimer: ReturnType<typeof setTimeout> | null = null;
  private attached = false;
  private generation = 0;

  constructor(
    private readonly fit: () => void,
    private readonly scheduleFrame: FrameScheduler = (callback) => {
      requestAnimationFrame(callback);
    },
  ) {}

  attach(container: HTMLElement): void {
    this.detach();
    this.attached = true;
    this.observer = new ResizeObserver(() => this.scheduleFit());
    this.observer.observe(container);
  }

  /** Fit once on the next frame, cancelling the work if this attachment ends. */
  fitNextFrame(): void {
    this.scheduleFrameForGeneration(this.generation);
  }

  detach(): void {
    this.attached = false;
    this.generation += 1;
    this.cancelTimers();
    this.observer?.disconnect();
    this.observer = null;
  }

  dispose(): void {
    this.detach();
  }

  private scheduleFit(): void {
    if (this.quietTimer === null) {
      this.maxWaitTimer = setTimeout(() => this.flush(), TERMINAL_RESIZE_MAX_WAIT_MS);
    } else {
      clearTimeout(this.quietTimer);
    }
    this.quietTimer = setTimeout(() => this.flush(), TERMINAL_RESIZE_QUIET_MS);
  }

  private flush(): void {
    this.cancelTimers();
    this.scheduleFrameForGeneration(this.generation);
  }

  private scheduleFrameForGeneration(generation: number): void {
    this.scheduleFrame(() => {
      if (this.attached && this.generation === generation) this.fit();
    });
  }

  private cancelTimers(): void {
    if (this.quietTimer !== null) {
      clearTimeout(this.quietTimer);
      this.quietTimer = null;
    }
    if (this.maxWaitTimer !== null) {
      clearTimeout(this.maxWaitTimer);
      this.maxWaitTimer = null;
    }
  }
}
