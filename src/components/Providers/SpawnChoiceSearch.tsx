import { useId, useLayoutEffect, useRef, useState, type KeyboardEvent, type RefObject } from 'react';
import type { SpawnOption } from '../../lib/groups';
import { offeredUnderLogin } from '../../lib/launchSelection';
import type { HarnessLoginStatus } from '../../lib/tauri/provider';
import { loginUnavailableReason } from './HarnessLoginMenu';
import { ProviderIcon } from './ProviderIcon';
import type { GroupedProviderMenuProps } from './GroupedProviderMenu';

type SpawnChoice = {
  key: string;
  providerId: string;
  configurationId?: string;
  loginId?: string;
  label: string;
  context?: string;
  icon: string;
  unavailable?: string | null;
  warning?: string | null;
};

function findChoices(query: string, parents: SpawnOption[], recipes: SpawnOption[], logins: HarnessLoginStatus[], decorate?: GroupedProviderMenuProps['decorate']): SpawnChoice[] {
  const terms = query.trim().toLowerCase().split(/\s+/);
  const choices: SpawnChoice[] = [];
  const add = (choice: SpawnChoice) => {
    const label = `${choice.context ?? ''} ${choice.label}`.toLowerCase();
    if (terms.every(term => label.includes(term))) choices.push(choice);
  };
  for (const parent of parents) {
    const icon = parent.provider_id ?? parent.harness_id;
    const addChoice = (row: Omit<SpawnChoice, 'key' | 'icon'>, rowIcon = icon) => {
      const key = JSON.stringify([row.providerId, row.configurationId, row.loginId]);
      add({ ...row, key, icon: rowIcon });
    };
    addChoice({ providerId: parent.id, label: parent.label, unavailable: parent.unavailable_reason, warning: parent.verification_warning });

    const addRecipes = (login?: HarnessLoginStatus) => {
      for (const row of recipes) {
        const recipe = row.configuration;
        if (!recipe || row.harness_id !== parent.harness_id || !offeredUnderLogin(recipe, login?.id)) continue;
        addChoice({
          providerId: recipe.spawn_option_id,
          configurationId: recipe.id,
          loginId: login?.id,
          label: recipe.name,
          context: login ? `${parent.label} · ${login.label}` : parent.label,
          unavailable: row.unavailable_reason ?? decorate?.(row).unavailable_reason,
          warning: row.verification_warning,
        }, row.provider_id ?? icon);
      }
    };
    const harnessLogins = parent.is_proxied ? [] : logins.filter(login => login.harness_id === parent.harness_id);
    if (logins.length <= 1 || harnessLogins.length === 0) {
      addRecipes();
      continue;
    }
    for (const login of harnessLogins) {
      const unavailable = loginUnavailableReason(login);
      addChoice({ providerId: parent.id, loginId: login.id, label: login.label, context: parent.label, unavailable: unavailable ?? parent.unavailable_reason });
      if (!unavailable) addRecipes(login);
    }
  }
  return choices;
}

export function SpawnChoiceSearch({
  inputRef, query, setQuery, parents, recipes, logins, decorate, mode, onSelect, onFocus, onBrowse, onClose, onManage, closeOnTab,
}: {
  inputRef: RefObject<HTMLInputElement | null>;
  query: string;
  setQuery: (value: string) => void;
  parents: SpawnOption[];
  recipes: SpawnOption[];
  logins: HarnessLoginStatus[];
  decorate?: GroupedProviderMenuProps['decorate'];
  mode: GroupedProviderMenuProps['mode'];
  onSelect: GroupedProviderMenuProps['onSelect'];
  onFocus: () => void;
  onBrowse: (last: boolean) => boolean;
  onClose?: () => void;
  onManage: () => void;
  closeOnTab: boolean;
}) {
  const listId = useId();
  const activeRef = useRef<HTMLLIElement>(null);
  const searching = query.trim().length > 0;
  const choices = searching ? findChoices(query, parents, recipes, logins, decorate) : [];
  const [navigation, setNavigation] = useState<{ query: string; key: string | undefined }>({ query, key: undefined });
  // Seed a new query once. Keep the active identity across catalogue refreshes
  // so deleting a recipe cannot silently retarget a pending Enter.
  if (navigation.query !== query) {
    setNavigation({ query, key: (choices.find(choice => !choice.unavailable) ?? choices[0])?.key });
  }
  const activeIndex = choices.findIndex(choice => choice.key === navigation.key);
  const expanded = choices.length > 0;
  const select = (choice: SpawnChoice | undefined, altKey: boolean) => {
    if (choice && !choice.unavailable) onSelect(choice.providerId, altKey, choice.configurationId, choice.loginId);
  };
  useLayoutEffect(() => {
    activeRef.current?.scrollIntoView?.({ block: 'nearest' });
  }, [activeIndex, navigation.key]);

  const closeFromTab = (event: KeyboardEvent) => {
    if (!closeOnTab || !onClose) return;
    event.stopPropagation();
    onClose();
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.nativeEvent.isComposing) {
      event.stopPropagation();
      return;
    }
    const { key, altKey, ctrlKey, metaKey, shiftKey } = event;
    if (key === 'Escape') {
      if (!query && !onClose) return;
      event.preventDefault();
      event.stopPropagation();
      if (query) setQuery('');
      else onClose?.();
      return;
    }
    if (key === 'Tab') {
      if (!shiftKey && !searching && onBrowse(false)) {
        event.preventDefault();
        event.stopPropagation();
      } else if (!shiftKey && searching && !expanded) {
        // Let Tab reach the recovery button without dismissing the parent menu.
        event.stopPropagation();
      } else {
        closeFromTab(event);
      }
      return;
    }
    // Keep caret movement inside the input, including in a nested Regenerate menu.
    event.stopPropagation();
    if (key === 'Enter') {
      event.preventDefault();
      if (!event.repeat) select(choices[activeIndex], altKey);
      return;
    }
    if (shiftKey || ctrlKey || metaKey || altKey) return;
    if (!searching && (key === 'ArrowDown' || key === 'ArrowUp')) {
      event.preventDefault();
      onBrowse(key === 'ArrowUp');
      return;
    }
    if (!expanded) return;
    let next: number;
    switch (key) {
      case 'ArrowDown': next = (activeIndex + 1) % choices.length; break;
      case 'ArrowUp': next = activeIndex <= 0 ? choices.length - 1 : activeIndex - 1; break;
      case 'Home': next = 0; break;
      case 'End': next = choices.length - 1; break;
      default: return;
    }
    event.preventDefault();
    setNavigation({ query, key: choices[next].key });
  };

  return <>
    <div className="border-b border-border-subtle bg-bg-overlay px-2 py-1.5">
      <input
        ref={inputRef}
        role="combobox"
        aria-label="Search launch choices"
        aria-expanded={expanded}
        aria-controls={expanded ? listId : undefined}
        aria-activedescendant={activeIndex >= 0 ? `${listId}-${activeIndex}` : undefined}
        aria-autocomplete="list"
        autoComplete="off"
        spellCheck={false}
        placeholder={mode === 'resume' ? 'Search providers…' : 'Search choices…'}
        value={query}
        onFocus={onFocus}
        onChange={event => setQuery(event.target.value)}
        onKeyDown={handleKeyDown}
        className="block min-h-6 w-full min-w-0 rounded-sm border border-border-default bg-bg-card px-2 py-1 text-sm text-text-primary outline-none placeholder:text-text-secondary focus:border-accent-cyan"
      />
    </div>
    {expanded && <ul id={listId} role="listbox" aria-label="Matching launch choices" className="max-h-[min(18rem,45vh)] overflow-y-auto">
      {choices.map((choice, index) => <li
        key={choice.key}
        id={`${listId}-${index}`}
        ref={index === activeIndex ? activeRef : undefined}
        role="option"
        aria-label={choice.context ? `${choice.label} ${choice.context}` : choice.label}
        aria-describedby={choice.unavailable || choice.warning ? `${listId}-${index}-reason` : undefined}
        aria-selected={index === activeIndex}
        aria-disabled={Boolean(choice.unavailable)}
        onMouseDown={event => event.preventDefault()}
        onClick={event => { event.stopPropagation(); select(choice, event.altKey); }}
        className={`flex min-h-6 items-start gap-2 px-3 py-1.5 text-sm text-text-primary ${index === activeIndex ? 'bg-bg-selection' : 'hover:bg-bg-card-hover'}`}
      >
        <ProviderIcon providerId={choice.icon} className="mt-0.5 h-3.5 w-3.5 shrink-0" />
        <span className="min-w-0 break-words">
          <span className="block">{choice.label}</span>
          {choice.context && <span className="block text-text-secondary">{choice.context}</span>}
          {(choice.unavailable || choice.warning) && <span id={`${listId}-${index}-reason`}>
            {choice.unavailable && <span className="block text-text-secondary">{choice.unavailable}</span>}
            {choice.warning && <span className="block text-status-warning">{choice.warning}</span>}
          </span>}
        </span>
      </li>)}
    </ul>}
    {searching && !expanded && <div className="px-3 py-2 text-sm">
      <p role="status" className="text-text-secondary">No matching launch choices.</p>
      <button type="button" onClick={onManage}
        onKeyDown={event => {
          if (event.key === 'Tab') {
            if (event.shiftKey) event.stopPropagation();
            else closeFromTab(event);
            return;
          }
          if (event.key !== 'Escape') return;
          event.preventDefault();
          event.stopPropagation();
          setQuery('');
          inputRef.current?.focus();
        }}
        className="mt-1 min-h-6 text-accent-cyan hover:underline">Manage launch configurations</button>
    </div>}
  </>;
}
