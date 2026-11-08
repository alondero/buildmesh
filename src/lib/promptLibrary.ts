import type { SavedPrompt } from '../types/generated/SavedPrompt';

// Built-ins stay immutable; the editor saves a personalized copy into preferences.
export const BUILT_IN_PROMPTS: readonly SavedPrompt[] = [
  { id: 'builtin:review', name: 'Review recent changes', body: 'Review the current changes against the project instructions and intended behavior. Inspect the relevant code and tests. Report actionable bugs and regressions with file locations, severity, and concrete failure scenarios. Separate required fixes from optional suggestions.' },
  { id: 'builtin:diagnose', name: 'Diagnose a failing check', body: 'Find the failing build, test, or lint check in this project. Reproduce the failure, trace its cause, and implement a focused fix. Run the affected checks and explain what changed and any remaining failures.' },
  { id: 'builtin:orient', name: 'Explore this project', body: 'Read the project instructions and inspect the codebase. Explain the architecture, main workflows, key modules, and how to build and test it. Highlight the best entry points for making changes.' },
  { id: 'builtin:tests', name: 'Find missing test coverage', body: 'Inspect the recent changes and their tests. Identify important behavior that is untested, prioritize realistic failure scenarios, and add focused regression tests. Run the affected tests and summarize the coverage added.' },
];

export function searchPrompts(prompts: readonly SavedPrompt[], query: string): SavedPrompt[] {
  const words = query.toLocaleLowerCase().trim().split(/\s+/);
  return prompts.filter(prompt => words.every(word => `${prompt.name}\n${prompt.body}`.toLocaleLowerCase().includes(word)));
}
