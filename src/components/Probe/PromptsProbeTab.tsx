import { useProbeContext } from '../../hooks/useProbeContext';
import { useMeshStore } from '../../stores/meshStore';
import { useProviderList } from '../../hooks/useProviderList';
import { PromptLibrary } from '../Prompts/PromptLibrary';

export { loadPrompts } from '../Prompts/PromptLibrary';

export function PromptsProbeTab() {
  const { activeMeshId } = useProbeContext();
  const mesh = useMeshStore(state => activeMeshId === null ? null : state.meshesById.get(activeMeshId) ?? null);
  const providers = useProviderList();
  return <PromptLibrary mesh={mesh} providers={providers} />;
}
