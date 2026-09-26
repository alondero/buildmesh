import { createServer } from 'node:http';

const MAX_HOOK_BYTES = 1024 * 1024;

export async function createCodexHookRelay() {
  let plan = [];
  let eventCounts = new Map();
  let nextId = 1;
  const events = [];
  const held = [];
  const waiters = new Set();

  const server = createServer((request, response) => {
    void handle(request, response);
  });

  async function handle(request, response) {
    const url = new URL(request.url ?? '/', 'http://127.0.0.1');
    if (request.method === 'GET' && url.pathname === '/__health') {
      return send(response, 200, { ok: true });
    }

    const match = request.method === 'POST'
      ? url.pathname.match(/^\/api\/attention\/(\d+)$/)
      : null;
    if (!match) return send(response, 404, { error: 'not found' });

    try {
      const body = await readBody(request);
      const payload = JSON.parse(body);
      const nodeId = Number(match[1]);
      const targetPort = Number(url.searchParams.get('forward_port'));
      if (!Number.isInteger(targetPort) || targetPort < 1 || targetPort > 65535) {
        return send(response, 400, { error: 'invalid forward port' });
      }
      const eventName = typeof payload.hook_event_name === 'string'
        ? payload.hook_event_name
        : typeof payload.hookName === 'string'
          ? payload.hookName
          : typeof payload.event === 'string' ? payload.event : 'unknown';
      const sessionId = payload.session_id ?? payload.sessionId ?? payload.sessionID ?? null;
      const turnId = payload.turn_id ?? payload.prompt_id ?? payload.promptId ?? null;
      const countKey = `${nodeId}:${eventName}`;
      const occurrence = (eventCounts.get(countKey) ?? 0) + 1;
      eventCounts.set(countKey, occurrence);
      const rule = plan.find((candidate) => !candidate.used
        && candidate.event === eventName
        && (candidate.occurrence === undefined || candidate.occurrence === occurrence)
        && (candidate.turnId === undefined || candidate.turnId === turnId));
      if (rule) rule.used = true;

      const record = {
        id: nextId++,
        nodeId,
        event: eventName,
        occurrence,
        sessionId,
        turnId,
        toolName: payload.tool_name ?? payload.toolName ?? null,
        action: rule?.action ?? 'forward',
        receivedAt: new Date().toISOString(),
        forwardCount: 0,
        forwardStatuses: [],
      };
      events.push(record);
      notify(record);

      if (rule?.action === 'drop') return send(response, 200, {});
      if (rule?.action === 'delay') {
        held.push({ record, body, targetPort, contentType: request.headers['content-type'] });
        return send(response, 200, {});
      }

      const copies = rule?.action === 'duplicate' ? 2 : 1;
      for (let copy = 0; copy < copies; copy += 1) {
        await forward({ record, body, targetPort, contentType: request.headers['content-type'] });
      }
      return send(response, 200, {});
    } catch (error) {
      return send(response, 200, { fixture_error: error.message });
    }
  }

  function notify(record) {
    for (const waiter of waiters) {
      if (waiter.predicate(record)) {
        clearTimeout(waiter.timer);
        waiters.delete(waiter);
        waiter.resolve(record);
      }
    }
  }

  async function forward(item) {
    const endpoint = `http://127.0.0.1:${item.targetPort}/api/attention/${item.record.nodeId}`;
    try {
      const forwarded = await fetch(endpoint, {
        method: 'POST',
        headers: { 'content-type': item.contentType ?? 'application/json' },
        body: item.body,
        signal: AbortSignal.timeout(15_000),
      });
      await forwarded.arrayBuffer();
      item.record.forwardCount += 1;
      item.record.forwardStatuses.push(forwarded.status);
      item.record.forwardedAt = new Date().toISOString();
    } catch (error) {
      item.record.forwardStatuses.push(`error: ${error.message}`);
    }
  }

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = server.address();
  const baseUrl = `http://127.0.0.1:${address.port}`;

  return {
    baseUrl,
    events,
    setPlan(rules) {
      plan = rules.map((rule) => ({ ...rule, used: false }));
      eventCounts = new Map();
    },
    waitFor(predicate, timeoutMs = 30_000) {
      const existing = events.find(predicate);
      if (existing) return Promise.resolve(existing);
      return new Promise((resolve, reject) => {
        const waiter = { predicate, resolve, reject, timer: null };
        waiter.timer = setTimeout(() => {
          waiters.delete(waiter);
          reject(new Error(`Timed out waiting for Codex hook in ${timeoutMs}ms`));
        }, timeoutMs);
        waiters.add(waiter);
      });
    },
    async releaseHeld(predicate = () => true) {
      const selected = [];
      for (let index = held.length - 1; index >= 0; index -= 1) {
        if (predicate(held[index].record)) selected.unshift(...held.splice(index, 1));
      }
      for (const item of selected) await forward(item);
      return selected.map(({ record }) => record);
    },
    heldEvents() {
      return held.map(({ record }) => record);
    },
    async close() {
      await new Promise((resolve) => server.close(resolve));
      for (const waiter of waiters) {
        clearTimeout(waiter.timer);
        waiter.reject(new Error('Codex hook relay closed'));
      }
      waiters.clear();
    },
  };
}

async function readBody(request) {
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size > MAX_HOOK_BYTES) throw new Error('hook payload exceeds relay limit');
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString('utf8');
}

function send(response, status, value) {
  const body = JSON.stringify(value);
  response.writeHead(status, {
    'content-type': 'application/json',
    'content-length': Buffer.byteLength(body),
    connection: 'close',
  });
  response.end(body);
}
