import { describe, it, expect } from 'vitest';
import { ESLint } from 'eslint';

describe('IPC import lint enforcement', () => {
  const eslint = new ESLint();
  async function errors(code: string, filePath = 'src/components/NewFacetConsumer.tsx') {
    const [result] = await eslint.lintText(code, { filePath });
    return result.messages.filter((message) => message.ruleId === 'no-restricted-imports');
  }

  it.each(['../../lib/tauri', '../../lib/tauri.ts', '../lib/tauri', './tauri'])(
    'rejects new compatibility-barrel imports from %s', async (module) => {
      expect(await errors(`import { listAgentNodes } from '${module}'; listAgentNodes();`)).toHaveLength(1);
    },
  );
  it('permits domain imports and Channel without permitting raw invoke or private transport access', async () => {
    expect(await errors("import { listAgentNodes } from '../../lib/tauri/agentNode'; listAgentNodes();")).toEqual([]);
    expect(await errors("import { Channel } from '@tauri-apps/api/core'; new Channel();")).toEqual([]);
    expect(await errors("import { invoke as raw } from '@tauri-apps/api/core'; raw('x');")).toHaveLength(1);
    expect(await errors("import * as core from '@tauri-apps/api/core'; core.invoke('x');")).toHaveLength(1);
    expect(await errors("import { _invoke } from '../../lib/tauri/_invoke'; _invoke('x');")).toHaveLength(1);
    expect(await errors("export { _invoke } from '../../lib/tauri/_invoke';")).toHaveLength(1);
  });
  it('allows legacy consumers while still prohibiting private transport access there', async () => {
    expect(await errors("import { listAgentNodes } from '../lib/tauri'; listAgentNodes();", 'src/stores/agentNodeStore.ts')).toEqual([]);
    expect(await errors("import { toHostPath } from './tauri'; toHostPath('/repo');", 'src/lib/fileDropPaste.ts')).toEqual([]);
    expect(await errors("import { _invoke } from '../lib/tauri/_invoke'; _invoke('x');", 'src/stores/agentNodeStore.ts')).toHaveLength(1);
    expect(await errors("import { _invoke } from './_invoke'; _invoke('x');", 'src/lib/tauri/agentNode.ts')).toEqual([]);
  });
  // Issue #2833 — the IPC *event* transport has one owner, so the renderer
  // health line can count the events the app receives. A direct `listen`
  // import would bypass that count silently.
  it('routes the IPC event transport through its one owner', async () => {
    expect(await errors("import { listen } from '@tauri-apps/api/event'; listen('x', () => {});")).toHaveLength(1);
    expect(await errors("import { listen } from '../../lib/tauri/eventListen'; listen('x', () => {});")).toEqual([]);
    // `emit` is outbound and stays unrestricted: the health line measures
    // what the app *receives*.
    expect(await errors("import { emit } from '@tauri-apps/api/event'; emit('x');")).toEqual([]);
    // The owner itself may import the raw transport.
    expect(await errors("import { listen } from '@tauri-apps/api/event';", 'src/lib/tauri/eventListen.ts')).toEqual([]);
  });
});
