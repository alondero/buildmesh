import type { SavedPrompt } from '../types/generated/SavedPrompt';

// Built-ins stay immutable; the editor saves a personalized copy into preferences.
export const BUILT_IN_PROMPTS: readonly SavedPrompt[] = [
  { id: 'builtin:review', name: 'Review project quality', body: 'Read the project instructions and review the code on this checkout for actionable bugs. Prioritize important workflows and inspect their tests. Report findings with file locations, severity, and concrete failure scenarios. Separate required fixes from optional suggestions. You are a fresh agent; no previous conversation or another agent’s changes are provided.' },
  { id: 'builtin:diagnose', name: 'Diagnose a failing check', body: 'Find the failing build, test, or lint check in this project. Reproduce the failure, trace its cause, and implement a focused fix. Run the affected checks and explain what changed and any remaining failures.' },
  { id: 'builtin:orient', name: 'Explore this project', body: 'Read the project instructions and inspect the codebase. Explain the architecture, main workflows, key modules, and how to build and test it. Highlight the best entry points for making changes.' },
  { id: 'builtin:tests', name: 'Find missing test coverage', body: 'Read the project instructions and inspect the code and tests on this checkout. Identify important behavior that is untested, prioritize realistic failure scenarios, and add focused regression tests. Run the affected tests and summarize the coverage added.' },
];

export function searchPrompts(prompts: readonly SavedPrompt[], query: string): SavedPrompt[] {
  const words = query.toLocaleLowerCase().trim().split(/\s+/);
  return prompts.filter(prompt => words.every(word => `${prompt.name}\n${prompt.body}`.toLocaleLowerCase().includes(word)));
}
