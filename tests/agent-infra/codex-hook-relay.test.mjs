import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createCodexHookRelay } from '../../scripts/codex-hook-relay.mjs';

test('Codex hook relay forwards, duplicates, omits, and releases delayed callbacks', async () => {
  const received = [];
  const upstream = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    received.push({ path: request.url, body: Buffer.concat(chunks).toString('utf8') });
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end('{}');
  });
  await new Promise((resolve) => upstream.listen(0, '127.0.0.1', resolve));
  const port = upstream.address().port;
  const relay = await createCodexHookRelay();
  const post = async (hook) => fetch(`${relay.baseUrl}/api/attention/44?forward_port=${port}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(hook),
  });
  const postAndCheck = async (hook) => {
    const response = await post(hook);
    assert.equal(response.status, 200);
  };

  try {
    relay.setPlan([
      { event: 'UserPromptSubmit', occurrence: 1, action: 'duplicate' },
      { event: 'Stop', occurrence: 1, action: 'drop' },
      { event: 'Stop', occurrence: 2, action: 'delay' },
    ]);

    await postAndCheck({ hook_event_name: 'UserPromptSubmit', sessionID: 'session', promptId: 'turn-1', prompt: 'private marker' });
    assert.equal(received.length, 2);
    assert.equal(relay.events[0].sessionId, 'session');
    assert.equal(relay.events[0].turnId, 'turn-1');

    await postAndCheck({ hook_event_name: 'Stop', session_id: 'session', turn_id: 'turn-1' });
    assert.equal(relay.events[1].action, 'drop');
    assert.equal(received.length, 2);

    await postAndCheck({ hook_event_name: 'Stop', session_id: 'session', turn_id: 'turn-2' });
    assert.equal(relay.heldEvents().length, 1);
    assert.equal(received.length, 2);
    const [released] = await relay.releaseHeld();
    assert.equal(released.forwardCount, 1);
    assert.equal(received.length, 3);
    assert.equal(received[2].path, '/api/attention/44');
    assert.equal(JSON.stringify(relay.events).includes('private marker'), false);
  } finally {
    await relay.close();
    await new Promise((resolve) => upstream.close(resolve));
  }
});
