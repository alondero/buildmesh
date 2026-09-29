interface EffortGuidanceProps {
  id: string;
  efforts: readonly string[];
  selectedEffort: string | null | undefined;
}

/** Explain model-dependent effort availability when max is supported. */
export function EffortGuidance({ id, efforts, selectedEffort }: EffortGuidanceProps) {
  if (!efforts.includes('max')) return null;

  return (
    <p id={id} className="text-text-secondary">
      Availability depends on the selected model.{selectedEffort === 'max' && ' Max applies to the launched session.'}
    </p>
  );
}
