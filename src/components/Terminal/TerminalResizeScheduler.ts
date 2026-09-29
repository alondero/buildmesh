/**
 * Owns the resize-observation policy shared by agent and build/run terminals.
 *
 * A ResizeObserver can fire once per layout frame while a split-pane handle
 * moves. Wait for a trailing quiet period so the terminal and PTY resize only
 * after the drag settles. The fit runs on the next animation frame, keeping
 * DOM measurement aligned with the browser render loop.
 */
export const TERMINAL_RESIZE_QUIET_MS = 200;

type FrameScheduler = (callback: () => void) => void;

export class TerminalResizeScheduler {
  private observer: ResizeObserver | null = null;
  private quietTimer: ReturnType<typeof setTimeout> | null = null;
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
    if (this.quietTimer !== null) {
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
  }
}
