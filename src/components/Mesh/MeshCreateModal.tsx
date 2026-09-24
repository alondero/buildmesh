import { formatError } from '../../lib/errorUtils';
import { useEffect, useRef, useState } from 'react';
import { Modal, ModalCloseButton } from '../shared/Modal';
import { MeshColorPicker } from './MeshColorPicker';
import { useMeshStore } from '../../stores/meshStore';
import { addToast } from '../../stores/toastStore';
import { pickMeshFolder } from '../../lib/tauri';
import { defaultMeshColor } from '../../lib/meshColors';
import { joinDisplayPath, repoNameFromInput } from '../../lib/githubRepo';
import type { PickedFolder } from '../../types/generated/PickedFolder';

interface MeshCreateModalProps {
  onClose: () => void;
  /**
   * Palette hex the colour picker starts on. Optional — when omitted
   * the modal derives its own default from the current mesh count
   * (`defaultMeshColor(meshes.length)`). The owning UI should pass an
   * explicit value only when the colour picker must mirror a
   * non-default palette seed (e.g. a programmatic open that pre-seeds
   * a specific colour for the next mesh).
   */
  defaultColor?: string;
}

/** Where a new mesh's repository comes from. */
type MeshSource = 'open' | 'clone';

const SECTION_LABEL = 'text-2xs uppercase tracking-wide text-text-muted mb-1.5';
const PICKER_BUTTON =
  'px-3 py-1.5 text-xs font-medium text-accent-cyan bg-accent-cyan/10 hover:bg-accent-cyan/20 border border-accent-cyan/20 rounded-md transition-colors disabled:opacity-50';
const PATH_PREVIEW =
  'mt-1.5 text-xs font-mono text-text-secondary bg-bg-card border border-border-subtle rounded-md px-2 py-1 break-all';

/**
 * The "New mesh" modal. Two sources:
 *
 * - **Open folder** — pick an existing repository root on disk.
 * - **Clone from GitHub** — give a repo (`owner/repo` or a github.com URL), pick
 *   a parent folder, and the backend clones into `<parent>/<repo>` and registers
 *   the result as a mesh in one step.
 *
 * Both share the colour picker. Name/base-ref are derived backend-side (the
 * clone flow resolves the repo's real default branch so the new mesh isn't a
 * drifted root).
 */
export function MeshCreateModal({ onClose, defaultColor }: MeshCreateModalProps) {
  const createMesh = useMeshStore((s) => s.createMesh);
  const cloneMesh = useMeshStore((s) => s.cloneMesh);
  const selectMesh = useMeshStore((s) => s.selectMesh);
  // The modal owns its own default-colour derivation (rather than the
  // parent reaching into `useMeshStore.getState().meshes.length`
  // inside the JSX render) so the abstraction boundary is correct —
  // App.tsx has no business calculating palette offsets. Senior-
  // review round 4: reading store state in JSX render was a known
  // anti-pattern (concurrent-render tear safety).
  const meshCount = useMeshStore((s) => s.meshes.length);
  const initialColor = defaultColor ?? defaultMeshColor(meshCount);

  const [source, setSource] = useState<MeshSource>('open');
  const [folder, setFolder] = useState<PickedFolder | null>(null);
  const [parent, setParent] = useState<PickedFolder | null>(null);
  const [repoInput, setRepoInput] = useState('');
  const [color, setColor] = useState(initialColor);
  const [picking, setPicking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const primaryButtonRef = useRef<HTMLButtonElement>(null);
  // A clone outlives the dialog when it is dismissed: the backend runs it to
  // completion on the blocking pool. Tracking mount state lets the awaited
  // completion *report* its outcome instead of driving a form the user can no
  // longer see (and instead of silently registering a mesh or dropping an error).
  const stillMounted = useRef(true);
  useEffect(() => () => {
    stillMounted.current = false;
  }, []);

  const repoName = repoNameFromInput(repoInput);
  const destination =
    source === 'clone' && parent && repoName ? joinDisplayPath(parent.path, repoName) : null;
  const canSubmit = source === 'open' ? !!folder : !!repoName && !!parent;
  const primaryLabel =
    source === 'open'
      ? busy
        ? 'Creating…'
        : 'Create mesh'
      : busy
        ? 'Cloning…'
        : 'Clone mesh';

  const switchSource = (next: MeshSource) => {
    if (next === source) return;
    setSource(next);
    // Drop any error from the other source so a stale message can't sit above
    // a form it no longer describes.
    setError(null);
  };

  const handleChooseFolder = async () => {
    setPicking(true);
    setError(null);
    try {
      const picked = await pickMeshFolder();
      // null means the user cancelled the OS dialog — leave the prior
      // selection (if any) untouched rather than clearing it.
      if (picked) {
        if (source === 'clone') setParent(picked);
        else setFolder(picked);
      }
    } catch (e) {
      setError(formatError(e));
    } finally {
      setPicking(false);
    }
  };

  const handleCreate = async () => {
    if (!folder || busy) return;
    setBusy(true);
    setError(null);
    const mesh = await createMesh(folder.name, folder.path, color);
    if (!stillMounted.current) return;
    setBusy(false);
    if (mesh) {
      selectMesh(mesh.id);
      onClose();
    } else {
      setError('Failed to create mesh. See logs for details.');
    }
  };

  const handleClone = async () => {
    if (!parent || !repoName || busy) return;
    // Capture the submitted target up front so the clone is pinned to exactly
    // what the user confirmed.
    const repo = repoInput.trim();
    const parentPath = parent.path;
    setBusy(true);
    setError(null);
    const result = await cloneMesh(repo, parentPath, color);
    if (!stillMounted.current) {
      // Dismissed mid-clone. The mesh row is already committed and the store has
      // appended it, so report the outcome rather than updating a form the user
      // can no longer see: a silent success would leave an unexplained mesh in
      // the sidebar, and a silent failure would be lost entirely.
      if ('mesh' in result) {
        addToast('Clone', `Cloned ${repo} into ${parentPath}`, 'success');
      } else {
        addToast('Clone', result.error, 'error');
      }
      return;
    }
    setBusy(false);
    if ('mesh' in result) {
      selectMesh(result.mesh.id);
      onClose();
    } else {
      setError(result.error);
    }
  };

  return (
    <Modal
      onClose={onClose}
      labelledBy="mesh-create-title"
      maxWidth="max-w-md"
      closeOnBackdrop={false}
      defaultFocusRef={primaryButtonRef}
    >
      <div className="flex items-start justify-between mb-4">
        <h2 id="mesh-create-title" className="text-sm font-semibold text-text-primary">
          New mesh
        </h2>
        <ModalCloseButton onClose={onClose} />
      </div>

      {/* Source */}
      <div
        role="group"
        aria-label="Mesh source"
        className="mb-4 flex gap-1 rounded-md border border-border-subtle bg-bg-card p-0.5"
      >
        <button
          type="button"
          aria-pressed={source === 'open'}
          onClick={() => switchSource('open')}
          disabled={busy}
          className={`flex-1 rounded-md px-3 py-1.5 text-xs font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${
            source === 'open'
              ? 'bg-accent-cyan/15 text-accent-cyan'
              : 'text-text-secondary hover:text-text-primary'
          }`}
        >
          Open folder
        </button>
        <button
          type="button"
          aria-pressed={source === 'clone'}
          onClick={() => switchSource('clone')}
          disabled={busy}
          className={`flex-1 rounded-md px-3 py-1.5 text-xs font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${
            source === 'clone'
              ? 'bg-accent-cyan/15 text-accent-cyan'
              : 'text-text-secondary hover:text-text-primary'
          }`}
        >
          Clone from GitHub
        </button>
      </div>

      {source === 'open' ? (
        /* Location */
        <div className="mb-4">
          <div className={SECTION_LABEL}>Location</div>
          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={handleChooseFolder}
              disabled={picking || busy}
              className={PICKER_BUTTON}
            >
              {picking ? 'Choosing…' : folder ? 'Change folder…' : 'Choose folder…'}
            </button>
            {folder && (
              <span className="text-xs font-medium text-text-primary truncate">{folder.name}</span>
            )}
          </div>
          {folder && <p className={PATH_PREVIEW}>{folder.path}</p>}
        </div>
      ) : (
        <>
          {/* Repository */}
          <div className="mb-4">
            <label htmlFor="mesh-clone-repo" className={`block ${SECTION_LABEL}`}>
              Repository
            </label>
            <input
              id="mesh-clone-repo"
              type="text"
              value={repoInput}
              onChange={(e) => setRepoInput(e.target.value)}
              onKeyDown={(e) => {
                // Enter submits, matching the desktop-modal convention (the
                // omnibar does the same). `handleClone` self-guards on an
                // incomplete form and on `busy`.
                if (e.key === 'Enter') {
                  e.preventDefault();
                  void handleClone();
                }
              }}
              disabled={busy}
              placeholder="owner/repo or https://github.com/owner/repo"
              spellCheck={false}
              autoComplete="off"
              className="w-full px-2 py-1.5 text-xs bg-bg-input border border-border-default rounded-md text-text-primary placeholder:text-text-muted focus:outline-none focus:border-accent-cyan transition-colors disabled:opacity-50"
            />
          </div>

          {/* Destination (parent folder) */}
          <div className="mb-4">
            <div className={SECTION_LABEL}>Destination</div>
            <div className="flex items-center gap-2">
              <button
                type="button"
                onClick={handleChooseFolder}
                disabled={picking || busy}
                className={PICKER_BUTTON}
              >
                {picking ? 'Choosing…' : parent ? 'Change folder…' : 'Choose parent folder…'}
              </button>
              {parent && (
                <span className="text-xs font-medium text-text-primary truncate">
                  {parent.name}
                </span>
              )}
            </div>
            {destination ? (
              <>
                <p className={PATH_PREVIEW}>{destination}</p>
                <p className="mt-1.5 text-2xs text-text-muted">
                  Cloned into a new {repoName} folder here.
                </p>
              </>
            ) : (
              parent && (
                <p className="mt-1.5 text-2xs text-text-muted">
                  Enter a repository above to see the destination path.
                </p>
              )
            )}
          </div>
        </>
      )}

      {/* Colour */}
      <div className="mb-5">
        <div id="mesh-create-color-label" className={SECTION_LABEL}>
          Colour
        </div>
        <MeshColorPicker value={color} onChange={setColor} labelId="mesh-create-color-label" />
      </div>

      {error && <p className="mb-3 text-xs text-status-error break-words">{error}</p>}

      <div className="flex justify-end gap-2">
        {/* Cancel stays enabled during a clone on purpose. Dismissal is made safe
            by the mount guard above (the completion reports via toast instead of
            writing into an unmounted form), and trapping the user inside the
            modal for up to the 10-minute clone timeout would be worse than
            letting them leave an operation they already started. */}
        <button
          type="button"
          onClick={onClose}
          className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
        >
          Cancel
        </button>
        <button
          ref={primaryButtonRef}
          type="button"
          onClick={source === 'open' ? handleCreate : handleClone}
          disabled={!canSubmit || busy}
          className="px-3 py-1.5 text-xs font-medium text-accent-cyan bg-accent-cyan/10 hover:bg-accent-cyan/20 border border-accent-cyan/20 rounded-md transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
        >
          {primaryLabel}
        </button>
      </div>
    </Modal>
  );
}
