import { describe, expect, it, vi } from 'vitest';
import { createStreamSubscription } from '../../src/lib/streams/streamSubscription';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

describe('stream subscription lifecycle', () => {
  it('does not let a retired rejection detach a newer registration', async () => {
    const old = deferred<() => void>();
    const current = deferred<() => void>();
    const attach = vi.fn().mockReturnValueOnce(old.promise).mockReturnValue(current.promise);
    const stream = createStreamSubscription<number>('test', attach);
    const releaseOld = stream.subscribe(vi.fn());
    releaseOld();
    const releaseCurrent = stream.subscribe(vi.fn());

    old.reject(new Error('retired registration failed'));
    await Promise.resolve();
    await Promise.resolve();
    const releaseAnother = stream.subscribe(vi.fn());
    expect(attach).toHaveBeenCalledTimes(2);

    const unlisten = vi.fn();
    current.resolve(unlisten);
    await Promise.resolve();
    releaseCurrent();
    releaseAnother();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it('fences retired notifications before and after late registration success', async () => {
    const old = deferred<() => void>();
    const callbacks: ((value: number) => void)[] = [];
    const currentUnlisten = vi.fn();
    const stream = createStreamSubscription<number>('test', notify => {
      callbacks.push(notify);
      return callbacks.length === 1 ? old.promise : Promise.resolve(currentUnlisten);
    });
    const releaseOld = stream.subscribe(vi.fn());
    releaseOld();
    const receive = vi.fn();
    const releaseCurrent = stream.subscribe(receive);

    callbacks[0](1);
    expect(receive).not.toHaveBeenCalled();
    const oldUnlisten = vi.fn();
    old.resolve(oldUnlisten);
    await Promise.resolve();
    expect(oldUnlisten).toHaveBeenCalledTimes(1);
    callbacks[0](2);
    callbacks[1](3);
    expect(receive.mock.calls).toEqual([[3]]);

    releaseCurrent();
    callbacks[1](4);
    expect(receive.mock.calls).toEqual([[3]]);
    expect(currentUnlisten).toHaveBeenCalledTimes(1);
  });

  it('claims registration before attach can synchronously subscribe another consumer', async () => {
    let releaseNested: (() => void) | undefined;
    const unlisten = vi.fn();
    const attach = vi.fn(() => {
      if (attach.mock.calls.length === 1) releaseNested = stream.subscribe(vi.fn());
      return Promise.resolve(unlisten);
    });
    const stream = createStreamSubscription<number>('test', attach);
    const release = stream.subscribe(vi.fn());
    await Promise.resolve();
    expect(attach).toHaveBeenCalledTimes(1);
    release();
    releaseNested?.();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it.each(['throw', 'reject'] as const)('retries a current registration failure (%s)', async failure => {
    const unlisten = vi.fn();
    const callbacks: ((value: number) => void)[] = [];
    const attach = vi.fn((notify: (value: number) => void) => {
      callbacks.push(notify);
      if (callbacks.length > 1) return Promise.resolve(unlisten);
      if (failure === 'throw') throw new Error('registration failed');
      return Promise.reject(new Error('registration failed'));
    });
    const stream = createStreamSubscription<number>('test', attach);
    const receive = vi.fn();
    const release = stream.subscribe(receive);
    await Promise.resolve();
    await Promise.resolve();
    const releaseAnother = stream.subscribe(vi.fn());
    await Promise.resolve();
    callbacks[0](1);
    callbacks[1](2);
    expect(receive.mock.calls).toEqual([[2]]);
    expect(attach).toHaveBeenCalledTimes(2);
    release();
    releaseAnother();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it('releases a late registration after reset without reviving notifications', async () => {
    const pending = deferred<() => void>();
    let notify!: (value: number) => void;
    const stream = createStreamSubscription<number>('test', callback => {
      notify = callback;
      return pending.promise;
    });
    const receive = vi.fn();
    stream.subscribe(receive);
    stream.reset();
    const unlisten = vi.fn();
    pending.resolve(unlisten);
    await Promise.resolve();
    notify(1);
    expect(receive).not.toHaveBeenCalled();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it('reports a throwing retired cleanup without clearing the current subscription', async () => {
    const old = deferred<() => void>();
    const current = deferred<() => void>();
    const attach = vi.fn().mockReturnValueOnce(old.promise).mockReturnValue(current.promise);
    const stream = createStreamSubscription<number>('test', attach);
    stream.subscribe(vi.fn())();
    const releaseCurrent = stream.subscribe(vi.fn());
    old.resolve(() => { throw new Error('retired cleanup failed'); });
    await Promise.resolve();
    await Promise.resolve();
    const releaseAnother = stream.subscribe(vi.fn());
    expect(attach).toHaveBeenCalledTimes(2);
    const unlisten = vi.fn();
    current.resolve(unlisten);
    await Promise.resolve();
    releaseCurrent();
    releaseAnother();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });
});
