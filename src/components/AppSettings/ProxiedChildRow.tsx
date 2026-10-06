import { useEffect, useState } from 'react';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { ProviderIcon } from '../Providers/ProviderIcon';
import * as api from '../../lib/tauri';
import type { ProviderPairing, PairingVerification, ModelTiers } from '../../lib/tauri';
import { EMPTY_TIERS, MODEL_TIER_FIELDS, SURFACE_LABEL } from './harnessVocabulary';

export function ProxiedChildRow({
  pairing,
  verifications,
  supportsWsl,
  harnessLabel,
  detachable,
  editing,
  onStartEdit,
  onCancelEdit,
  onSaveEdit,
  onDetach,
  onVerify,
  accountName,
  busy,
}: {
  pairing: ProviderPairing;
  verifications: PairingVerification[];
  supportsWsl: boolean;
  harnessLabel: string;
  detachable: boolean;
  editing: boolean;
  onStartEdit: () => void;
  onCancelEdit: () => void;
  onSaveEdit?: (baseUrl: string | null, modelTiers: ModelTiers | null) => Promise<void>;
  onDetach: (providerId: string) => void;
  onVerify: (envType: api.EnvType) => Promise<void>;
  accountName: (id: string) => string;
  busy: boolean;
}) {
  const { setNodeRef, transform, transition, isDragging, attributes, listeners } =
    useSortable({ id: pairing.provider_id, disabled: !detachable || editing || busy });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : 1,
  };

  const [editUrl, setEditUrl] = useState(pairing.base_url ?? '');
  const [editTiers, setEditTiers] = useState<ModelTiers>(pairing.model_tiers ?? EMPTY_TIERS);
  // `busy` is the parent's gate (which now folds in the issue-#1523
  // corruption lock), so the row's own Save follows the same lock.
  const [savingEdit, setSaving] = useState(false);
  const saving = savingEdit || busy;
  const [verifying, setVerifying] = useState<api.EnvType | null>(null);

  useEffect(() => {
    if (editing) {
      setEditUrl(pairing.base_url ?? '');
      setEditTiers(pairing.model_tiers ?? EMPTY_TIERS);
    }
  }, [editing, pairing]);

  const handleProps = detachable && !editing ? { ...attributes, ...listeners } : {};
  const showTiers = pairing.surface === 'anthropic';
  const modelFields = showTiers ? MODEL_TIER_FIELDS : MODEL_TIER_FIELDS.slice(0, 1);

  const saveEdit = async () => {
    if (!onSaveEdit || !editUrl.trim()) return;
    setSaving(true);
    try {
      await onSaveEdit(editUrl.trim(), editTiers);
    } catch {
      // Parent surfaces error.
    } finally {
      setSaving(false);
    }
  };

  const verify = async (envType: api.EnvType) => {
    setVerifying(envType);
    try {
      await onVerify(envType);
    } finally {
      setVerifying(null);
    }
  };

  const statusLabel = (verification?: PairingVerification) => verification?.status === 'stale'
    ? 'Reverification required'
    : verification?.status === 'failed'
      ? 'Verification failed'
      : verification?.status === 'unsupported'
        ? 'Unsupported'
        : verification?.status === 'verified'
          ? 'Verified'
          : 'Verifying';
  const displayedVerifications = pairing.surface === 'openai'
    ? verifications
    : verifications.slice(0, 1);

  return (
    <li
      ref={setNodeRef}
      style={style}
      className="border border-border-subtle rounded-md px-3 py-2"
      data-testid={`pairing-${pairing.harness_id}-${pairing.provider_id}`}
      data-spawn-harness={pairing.harness_id}
      data-spawn-id={`${pairing.harness_id}:${pairing.provider_id}`}
    >
      <div className="flex items-center gap-3">
        {detachable ? (
          <span
            {...handleProps}
            tabIndex={0}
            role="button"
            aria-roledescription="sortable"
            aria-label={`Reorder ${accountName(pairing.provider_id)} under ${harnessLabel}`}
            className="text-text-muted hover:text-text-secondary cursor-grab active:cursor-grabbing text-2xs select-none focus:outline-none focus-visible:ring-1 focus-visible:ring-accent-cyan rounded-sm"
            title="Drag to reorder"
          >
            ⋮⋮
          </span>
        ) : (
          <span className="w-3.5 text-transparent select-none" aria-hidden="true">
            ⋮⋮
          </span>
        )}
        <ProviderIcon providerId={pairing.provider_id} className="h-5 w-5" />
        <div className="min-w-0 flex-1">
          <div className="text-base text-text-primary truncate">
            {accountName(pairing.provider_id)}
            <span className="ml-2 text-sm text-text-secondary">{SURFACE_LABEL[pairing.surface]}</span>
          </div>
          {!editing && pairing.base_url && (
            <div className="text-sm text-text-secondary truncate font-mono">{pairing.base_url}</div>
          )}
          {!editing && (
            <div className="text-sm mt-1 space-y-1">
              {displayedVerifications.map((verification) => (
                <div key={verification.runtime}>
                  {pairing.surface === 'openai' && (
                    <span className="mr-2 text-text-muted">
                      {verification.runtime.startsWith('wsl:') || verification.runtime === 'wsl'
                        ? 'WSL'
                        : 'Native'}
                    </span>
                  )}
                  <span className={verification.status === 'verified' ? 'text-status-success' : 'text-status-warning'}>
                    {statusLabel(verification)}
                  </span>
                  {verification.reason && (
                    <span className="ml-2 text-text-muted">{verification.reason}</span>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>
        {detachable && !editing && (
          <>
            {pairing.surface === 'openai' && (
              <div className="flex gap-1">
                {(supportsWsl ? ['windows', 'wsl'] as const : ['windows'] as const).map((envType) => (
                  <button
                    key={envType}
                    onClick={() => verify(envType)}
                    disabled={busy || verifying !== null}
                    className="px-2 py-1 text-sm text-text-secondary hover:text-text-primary disabled:opacity-50"
                    aria-label={`Verify ${envType === 'wsl' ? 'WSL' : 'native'} ${accountName(pairing.provider_id)} under ${harnessLabel}`}
                  >
                    {verifying === envType
                      ? 'Verifying…'
                      : envType === 'wsl' ? 'Verify WSL' : 'Verify native'}
                  </button>
                ))}
              </div>
            )}
            {onSaveEdit && (
              <button
                onClick={onStartEdit}
                disabled={busy}
                className="px-3 py-1 text-sm text-text-secondary hover:text-text-primary disabled:opacity-50"
                aria-label={`Edit ${accountName(pairing.provider_id)} under ${harnessLabel}`}
              >
                Edit
              </button>
            )}
            <button
              onClick={() => onDetach(pairing.provider_id)}
              disabled={busy}
              className="px-3 py-1 bg-status-error/15 text-status-error text-sm rounded-md hover:bg-status-error/25 disabled:opacity-50"
              aria-label={`Detach ${accountName(pairing.provider_id)} from ${harnessLabel}`}
            >
              Detach
            </button>
          </>
        )}
      </div>

      {editing && (
        <div className="mt-3 space-y-3 border-t border-border-subtle pt-3">
          <div>
            <label className="block text-sm text-text-muted mb-1">Base URL</label>
            <input
              type="text"
              value={editUrl}
              onChange={(e) => setEditUrl(e.target.value)}
              className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
              aria-label={`Edit base URL for ${accountName(pairing.provider_id)}`}
            />
          </div>
          <div className="space-y-2">
              {modelFields.map(({ key: tk, label }) => (
                <div key={tk} className="flex items-center gap-3">
                  <span className="w-28 shrink-0 text-sm text-text-muted">{label}</span>
                  <input
                    type="text"
                    value={editTiers[tk] ?? ''}
                    onChange={(e) =>
                      setEditTiers((prev) => ({ ...prev, [tk]: e.target.value || null }))
                    }
                    className="flex-1 min-w-0 bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
                    aria-label={`Edit ${label} model for ${accountName(pairing.provider_id)}`}
                  />
                </div>
              ))}
          </div>
          <div className="flex gap-3">
            <button
              onClick={saveEdit}
              disabled={saving || !editUrl.trim() || !editTiers.default?.trim()}
              className="px-5 py-2 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-base rounded-md hover:bg-bg-card-hover disabled:opacity-50"
            >
              {savingEdit ? 'Saving…' : 'Save'}
            </button>
            <button
              onClick={onCancelEdit}
              disabled={saving}
              className="px-5 py-2 text-base text-text-secondary border border-border-strong rounded-md hover:bg-bg-card-hover hover:text-text-primary disabled:opacity-50"
            >
              Cancel
            </button>
          </div>
        </div>
      )}
    </li>
  );
}
