import { useId, useLayoutEffect, useRef, useState } from 'react';

type ModelOption = { value: string; label: string };

export function LaunchModelPicker({ value, options, onSelect }: {
  value: string;
  options: ModelOption[];
  onSelect: (value: string) => void;
}) {
  const id = useId();
  const listId = `${id}-models`;
  const trigger = useRef<HTMLButtonElement>(null);
  const activeOption = useRef<HTMLLIElement>(null);
  const [search, setSearch] = useState<{ query: string; activeValue?: string } | null>(null);
  const selectedLabel = options.find(option => option.value === value)?.label ?? value;
  const terms = search?.query.trim().toLowerCase().split(/\s+/) ?? [];
  const matches = search ? options.filter(option => {
    const label = option.label.toLowerCase();
    return terms.every(term => label.includes(term));
  }) : [];
  const activeIndex = search?.activeValue === undefined
    ? (matches.length ? 0 : -1)
    : matches.findIndex(option => option.value === search.activeValue);
  const activeValue = matches[activeIndex]?.value;

  useLayoutEffect(() => {
    activeOption.current?.scrollIntoView?.({ block: 'nearest' });
  }, [activeIndex, activeValue]);

  const close = () => {
    trigger.current?.focus();
    setSearch(null);
  };
  const pick = (option: ModelOption) => {
    if (option.value !== value) onSelect(option.value);
    close();
  };

  return <div className="launch-config-model" onBlur={event => {
    if (!event.currentTarget.contains(event.relatedTarget)) setSearch(null);
  }}>
    <label htmlFor={id}>Model</label>
    <button type="button" id={id} ref={trigger} aria-expanded={search !== null}
      aria-controls={search ? `${id}-search` : undefined} aria-describedby={`${id}-value`}
      tabIndex={search ? -1 : undefined} className="launch-config-model-trigger"
      onMouseDown={event => { if (search) event.preventDefault(); }}
      onClick={() => search ? close() : setSearch({ query: '', activeValue: value })}
      onKeyDown={event => {
        if (search || (event.key !== 'ArrowDown' && event.key !== 'ArrowUp')) return;
        event.preventDefault();
        event.stopPropagation();
        setSearch({ query: '', activeValue: value });
      }}>
      <span id={`${id}-value`}>{selectedLabel}</span>
    </button>
    {search && <div id={`${id}-search`} className="launch-config-model-search">
      <input autoFocus role="combobox" aria-label="Search models" aria-autocomplete="list"
        aria-expanded={true} aria-controls={listId}
        aria-activedescendant={activeIndex >= 0 ? `${listId}-${activeIndex}` : undefined}
        autoComplete="off" spellCheck={false} placeholder="Search models…" value={search.query}
        onChange={event => setSearch({ query: event.target.value })}
        onKeyDown={event => {
          if (event.key === 'Tab') return;
          event.stopPropagation();
          if (event.nativeEvent.isComposing) return;
          if (event.key === 'Escape') {
            event.preventDefault();
            if (search.query) setSearch({ query: '', activeValue: value });
            else close();
          } else if (event.key === 'Enter') {
            event.preventDefault();
            if (!event.repeat && matches[activeIndex]) pick(matches[activeIndex]);
          } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
            event.preventDefault();
            if (!matches.length) return;
            const next = event.key === 'ArrowDown'
              ? (activeIndex + 1) % matches.length
              : activeIndex <= 0 ? matches.length - 1 : activeIndex - 1;
            setSearch({ ...search, activeValue: matches[next].value });
          }
        }}
      />
      <ul id={listId} role="listbox" aria-label="Models" tabIndex={-1}>
        {matches.map((option, index) => <li key={option.value} id={`${listId}-${index}`}
          ref={index === activeIndex ? activeOption : undefined} role="option"
          aria-selected={index === activeIndex}
          onMouseDown={event => event.preventDefault()}
          onClick={() => pick(option)}>
          {option.label}
        </li>)}
      </ul>
      {!matches.length && <p role="status">No matching models.</p>}
    </div>}
  </div>;
}
