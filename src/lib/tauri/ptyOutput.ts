import { Channel } from '@tauri-apps/api/core';
import { _invoke } from './_invoke';

/**
 * Coerce a Tauri Channel raw-binary message into a `Uint8Array`.
 *
 * Raw frames arrive as an `ArrayBuffer`; older Tauri fetch paths may
 * deliver a `Response` for larger frames. Tests and older runtimes may
 * hand us a `Uint8Array` or another `ArrayBufferView` directly.
 * This helper recognizes direct bytes; the subscriber reads Response
 * bodies separately. JSON output events remain test injection only.
 */
export function bytesFromChannelMessage(message: unknown): Uint8Array | null {
  if (message instanceof Uint8Array) return message;
  if (message instanceof ArrayBuffer) return new Uint8Array(message);
  if (ArrayBuffer.isView(message)) {
    const view = message as ArrayBufferView;
    return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
  }
  return null;
}

interface ChannelResponse {
  arrayBuffer: () => Promise<ArrayBuffer>;
  body?: ReadableStream<Uint8Array> | null;
}

function isChannelResponse(message: unknown): message is ChannelResponse {
  if (typeof message !== 'object' || message === null) return false;
  return typeof (message as { arrayBuffer?: unknown }).arrayBuffer === 'function';
}

/** Own the stream reader so cancellation can interrupt a pending body read. */
function readChannelBody(message: ChannelResponse) {
  if (message.body) {
    const reader = message.body.getReader();
    let cancelled = false;
    let released = false;
    const promise = (async (): Promise<ArrayBuffer | null> => {
      const buffer = new Uint8Array(32 * 1024);
      let size = 0;
      try {
        while (!cancelled) {
          const chunk = await reader.read();
          if (cancelled) return null;
          if (chunk.done) return buffer.slice(0, size).buffer;
          if (size + chunk.value.length > buffer.length) throw new Error('PTY frame exceeds 32 KiB');
          buffer.set(chunk.value, size);
          size += chunk.value.length;
        }
        return null;
      } finally {
        void reader.cancel().catch(() => {});
        reader.releaseLock();
        released = true;
      }
    })();
    return { promise, cancel: () => {
      cancelled = true;
      if (!released) void reader.cancel().catch(() => {});
    } };
  }
  // Older/mocked runtimes expose only arrayBuffer. Detach the obsolete await
  // on cancellation; a non-cancellable foreign promise may still settle later.
  let cancel!: () => void;
  const cancelled = new Promise<null>(resolve => { cancel = () => resolve(null); });
  return { promise: Promise.race([message.arrayBuffer(), cancelled]), cancel };
}

type PtyOutputCommand = 'subscribe_agent_output' | 'subscribe_build_run_output';

interface OutputSubscription {
  channelId: number;
  cancel: () => void;
  ready?: Promise<void>;
  registered: boolean;
}
const subscriptionOwners = new WeakMap<Promise<void>, OutputSubscription>();
const outputSubscriptions = new Map<PtyOutputCommand, Map<number, OutputSubscription>>();

/** The producer admits at most 64 frames / 256 KiB, including Tauri storage.
 * Acknowledgements follow ordered delivery to TerminalWriter, not Channel arrival.
 */
export function subscribeRawPtyOutput(
  command: PtyOutputCommand,
  sessionId: number,
  onChunk: (data: Uint8Array) => void,
): Promise<void> {
  if (typeof Channel !== 'function') return Promise.resolve();
  const channel = new Channel<unknown>();
  let subscriptions = outputSubscriptions.get(command);
  if (!subscriptions) {
    subscriptions = new Map();
    outputSubscriptions.set(command, subscriptions);
  }
  subscriptions.get(sessionId)?.cancel();
  let consumer: typeof onChunk | null = onChunk;
  let queue: unknown[] = [];
  let reading = false;
  let sequence = 0;
  let acknowledged = 0;
  let acknowledging = false;
  let ackScheduled = false;
  let cancelBody: (() => void) | null = null;
  const isBuildRun = command === 'subscribe_build_run_output';
  const unsubscribe = isBuildRun ? 'unsubscribe_build_run_output' : 'unsubscribe_agent_output';
  const cancel = () => {
    consumer = null;
    queue = [];
    cancelBody?.();
    cancelBody = null;
    channel.onmessage = () => {};
  };
  const subscription: OutputSubscription = { channelId: channel.id, cancel, registered: false };
  subscriptions.set(sessionId, subscription);
  const fail = (error: unknown) => {
    console.error(`[PTY] failed to decode ${command} Channel frame:`, error);
    cancel();
    if (subscriptions.get(sessionId) === subscription) subscriptions.delete(sessionId);
    // Never return credit for unread bytes: terminate this failed subscription.
    void _invoke(unsubscribe, { sessionId, channelId: channel.id }).catch(() => {});
  };
  const acknowledge = () => {
    ackScheduled = false;
    if (!consumer || acknowledging || sequence === acknowledged) return;
    acknowledging = true;
    const completed = sequence;
    void _invoke('acknowledge_pty_output', {
      sessionId, channelId: channel.id, sequence: completed, buildRun: isBuildRun,
    }).then(() => {
      acknowledged = completed;
    }).catch(fail).finally(() => {
      acknowledging = false;
      if (consumer && sequence !== acknowledged) scheduleAck();
    });
  };
  const scheduleAck = () => {
    if (ackScheduled || acknowledging) return;
    ackScheduled = true;
    queueMicrotask(acknowledge);
  };
  const deliver = (bytes: Uint8Array) => {
    if (!consumer) return;
    consumer(bytes);
    sequence++;
    scheduleAck();
  };
  const drain = async () => {
    reading = true;
    try {
      while (consumer && queue.length > 0) {
        const message = queue.shift();
        const bytes = bytesFromChannelMessage(message);
        if (bytes) deliver(bytes);
        else if (isChannelResponse(message)) {
          const body = readChannelBody(message);
          cancelBody = body.cancel;
          const buffer = await body.promise;
          cancelBody = null;
          if (buffer) deliver(new Uint8Array(buffer));
        } else throw new Error('Invalid raw PTY Channel frame');
      }
    } catch (error) {
      if (consumer) fail(error);
    } finally {
      reading = false;
    }
  };
  channel.onmessage = (message) => {
    if (!consumer) return;
    const bytes = bytesFromChannelMessage(message);
    if (bytes && !reading) {
      try { deliver(bytes); } catch (error) { fail(error); }
      return;
    }
    queue.push(message);
    if (!reading) void drain();
  };
  const ready = _invoke<void>(command, { sessionId, onChunk: channel })
    .then(() => { subscription.registered = true; })
    .catch((error: unknown) => {
      cancel();
      if (subscriptions.get(sessionId) === subscription) subscriptions.delete(sessionId);
      throw error;
    });
  subscription.ready = ready;
  subscriptionOwners.set(ready, subscription);
  return ready;
}

export function unsubscribeRawPtyOutput(command: PtyOutputCommand, sessionId: number, owner?: Promise<void>): Promise<void> {
  const subscriptions = outputSubscriptions.get(command);
  const subscription = owner ? subscriptionOwners.get(owner) : subscriptions?.get(sessionId);
  if (owner && !subscription) return Promise.resolve();
  subscription?.cancel();
  if (subscriptions?.get(sessionId) === subscription) subscriptions?.delete(sessionId);
  const unsubscribe = command === 'subscribe_agent_output'
    ? 'unsubscribe_agent_output' : 'unsubscribe_build_run_output';
  const release = () => _invoke<void>(unsubscribe, {
    sessionId, ...(subscription ? { channelId: subscription.channelId } : {}),
  });
  return subscription?.ready && !subscription.registered
    ? subscription.ready.then(release, () => {}) : release();
}
