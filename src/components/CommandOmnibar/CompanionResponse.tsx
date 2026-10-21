import ReactMarkdown, { defaultUrlTransform } from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { focusNode } from '../../lib/navigation';
import { useUIStore } from '../../stores/uiStore';
import { useCompanionStore } from '../../stores/companionStore';
import { SafeLink } from '../shared/SafeLink';

export default function CompanionResponse({ response }: { response: string }) {
  return (
          <ReactMarkdown remarkPlugins={[remarkGfm]} urlTransform={url => /^buildmesh:\/\/(grid|node\/\d+)$/.test(url) ? url : defaultUrlTransform(url)}
            components={{ a: ({ href, children }) => {
              if (href && /^buildmesh:\/\/(grid|node\/\d+)$/.test(href)) return <button type="button" className="text-accent-violet underline" onClick={() => {
                if (href === 'buildmesh://grid') useUIStore.getState().setViewMode('mesh');
                else if (!focusNode(Number(href.slice('buildmesh://node/'.length)), { view: 'single', scope: 'node' })) {
                  useCompanionStore.setState({ error: 'This node is no longer available.' });
                  return;
                }
                useUIStore.getState().closeOmnibar();
              }}>{children}</button>;
              return <SafeLink url={href ?? ''}>{children}</SafeLink>;
            } }}>{response}</ReactMarkdown>
  );
}
