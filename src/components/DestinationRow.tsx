import type { ComponentType, ComponentPropsWithoutRef } from 'react';

type DestinationRowProps = Omit<ComponentPropsWithoutRef<'button'>, 'children' | 'title'> & {
  icon: ComponentType<{ className?: string }>;
  label: string;
  description?: string;
  active?: boolean;
};

/** Presentation only: the disclosure or combobox keeps ownership of focus. */
export function DestinationRow({ icon: Icon, label, description, active = false, className = '', ...props }: DestinationRowProps) {
  return (
    <button
      {...props}
      type="button"
      title={description ? `${label}: ${description}` : label}
      aria-description={description}
      className={`flex min-h-[24px] w-full items-center gap-2 rounded-md border-l-2 px-2 py-1 text-left text-label transition-colors ${
        active
          ? 'border-l-accent-cyan bg-bg-selection text-text-primary'
          : 'border-l-transparent text-text-primary hover:bg-bg-card-hover'
      } ${className}`}
    >
      <Icon className="h-4 w-4 shrink-0 text-text-secondary" />
      <span className="min-w-0 flex-1 truncate font-medium">{label}</span>
    </button>
  );
}
