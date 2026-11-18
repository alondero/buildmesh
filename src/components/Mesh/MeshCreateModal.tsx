import { formatError } from '../../lib/errorUtils';
import { useEffect, useRef, useState } from 'react';
import { Modal, ModalCloseButton } from '../shared/Modal';
import { MeshColorPicker } from './MeshColorPicker';
import { MeshCreateFailurePanel } from './MeshCreateFailurePanel';
import { enterMesh } from '../../lib/navigation';
import { useMeshStore } from '../../stores/meshStore';
import { useCloneActivityStore } from '../../stores/cloneActivityStore';
import { addToast } from '../../stores/toastStore';
import { pickMeshFolder } from '../../lib/tauri';
import { defaultMeshColor } from '../../lib/meshColors';
import { joinDisplayPath, repoNameFromInput } from '../../lib/githubRepo';
import { classifyMeshCreateFailure } from '../../lib/meshCreateFailure';
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

/**
 * An inline failure the dialog can show.
 *
 * `message` carries the backend's own text (issue #2794). `hint` and `detail`
 * are additions: the hint names the next step, the detail block carries the
 * diagnostics for a copyable report. The classification that produces them is
 * in `src/lib/meshCreateFailure.ts`; it leaves the message as it is.
 */
interface DialogFailure {
  message: string;
  hint?: string;
  detail?: string;
  /** The recovery actions this cause supports. */
  recoveries?: Array<'retry' | 'change-folder'>;
}

const SECTION_LABEL = 'text-2xs uppercase tracking-wide text-text-muted mb-1.5';
const PICKER_BUTTON =
  'px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary hover:bg-bg-card-hover border border-border-strong rounded-md transition-colors disabled:opacity-50';
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
 *
 * The clone flow does NOT own its operation. A clone outlives this dialog —
 * the backend runs it to completion and dismissing the dialog cannot stop it
 * (issue #2795) — so the operation lives in `cloneActivityStore` and this
 * dialog is one viewer of it. That is why the busy state says "Hide progress"
 * rather than "Cancel": there is no cancellation here to promise.
 */
export function MeshCreateModal({ onClose, defaultColor }: MeshCreateModalProps) {
  const createMesh = useMeshStore((s) => s.createMesh);
  const startClone = useCloneActivityStore((s) => s.startClone);
  const claimOperation = useCloneActivityStore((s) => s.claimOperation);
  const releaseOperation = useCloneActivityStore((s) => s.releaseOperation);
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
  // The open-folder flow is a single quick IPC, so its busy state lives here.
  // The clone flow's busy state comes from its operation (below), which has
  // its own owner.
  const [creating, setCreating] = useState(false);
  // The failure the dialog is showing, if any. It carries the message the
  // store returned for this request (issue #2794).
  const [failure, setFailure] = useState<DialogFailure | null>(null);
  // Asks the focus effect to put the focus back on the folder picker once the
  // native picker has closed.
  const [restorePickerFocus, setRestorePickerFocus] = useState(false);
  // The clone this dialog started (or joined). The operation itself lives
  // in the clone-activity store, so it survives this dialog being closed.
  const [cloneOperationId, setCloneOperationId] = useState<string | null>(null);
  // True when this submission JOINED a clone that was already running
  // instead of starting one. The reconciliation is visible rather than
  // silent: the user asked for a clone, and something else is already
  // doing it (issue #2795).
  const [joinedExisting, setJoinedExisting] = useState(false);
  const cloneOperation = useCloneActivityStore((s) =>
    cloneOperationId ? (s.operations.find((op) => op.id === cloneOperationId) ?? null) : null
  );
  const cloning = cloneOperation?.status === 'running';
  // Issue #2463: initial focus targets the folder picker, the first required
  // step of either flow. The submit button cannot take focus at mount — it is
  // disabled until the form is complete, and `.focus()` on a disabled button
  // is a no-op that leaves focus on the trigger behind the dialog.
  const folderButtonRef = useRef<HTMLButtonElement>(null);
  // A creation request in flight. `busy` is React state, so two clicks in the
  // same tick would both read the pre-update value and start a second
  // request; the ref is synchronous, so the second one is refused. A
  // duplicated creation would register the same folder twice (issue #2794).
  const createInFlight = useRef(false);
  // The failure panel. It takes focus when a failure appears, because the
  // submit control that failed is disabled while the request runs and a
  // disabled control does not hold the focus — without this the focus would
  // fall out of the dialog onto the page behind it (issue #2794).
  const failureRef = useRef<HTMLDivElement>(null);
  // A clone outlives the dialog when it is dismissed: the backend runs it to
  // completion on the blocking pool. Tracking mount state lets the awaited
  // completion *report* its outcome instead of driving a form the user can no
  // longer see (and instead of silently registering a mesh or dropping an error).
  const stillMounted = useRef(true);
  useEffect(() => {
    stillMounted.current = true;
    return () => { stillMounted.current = false; };
  }, []);
  // The operation this dialog is showing. Claiming it synchronously at submit
  // time (not in an effect) matters: the outcome is reported where it is
  // visible, and an effect would run after the clone had already settled.
  const claimedOperationId = useRef<string | null>(null);
  useEffect(() => {
    return () => {
      // The dialog is gone: nothing is showing this operation's outcome, so
      // the store reports it (a toast) instead of leaving it unsaid.
      if (claimedOperationId.current) {
        releaseOperation(claimedOperationId.current);
        claimedOperationId.current = null;
      }
    };
  }, [releaseOperation]);
  // The outcome of the operation this dialog started is acted on exactly
  // once, even though the effect re-runs whenever the operation changes.
  const handledOperationId = useRef<string | null>(null);

  // A failure is announced by the panel's `role="alert"`, and the focus is
  // moved onto it so the keyboard user stays inside the dialog and lands on
  // the message with the recovery actions right below it.
  useEffect(() => {
    if (failure) failureRef.current?.focus();
  }, [failure]);

  // The native folder picker takes the window's focus while it is open, and
  // the failure panel that offered it is gone by the time it closes. This flag
  // asks the effect below to put the focus back on a control inside the
  // dialog, so the keyboard user is not dropped on the page behind it. The
  // effect — not a timer — does the work, because the picker button is
  // disabled until React re-renders and `.focus()` on a disabled control is a
  // no-op.
  useEffect(() => {
    if (!restorePickerFocus || picking) return;
    setRestorePickerFocus(false);
    // A failure raised by the picker itself keeps the focus: its panel took
    // it, and it says what went wrong.
    if (!failureRef.current) folderButtonRef.current?.focus();
  }, [restorePickerFocus, picking]);

  const repoName = repoNameFromInput(repoInput);
  const destination =
    source === 'clone' && parent && repoName ? joinDisplayPath(parent.path, repoName) : null;
  const canSubmit = source === 'open' ? !!folder : !!repoName && !!parent;
  // The open-folder flow is a single quick IPC; the clone flow is a long
  // backend operation with its own owner.
  const busy = source === 'open' ? creating : cloning;
  const primaryLabel =
    source === 'open'
      ? creating
        ? 'Creating…'
        : 'Create mesh'
      : cloning
        ? 'Cloning…'
        : 'Clone mesh';
  // While a clone runs, dismissing this dialog HIDES the progress — the
  // clone keeps running. The button must not say "Cancel", because nothing
  // here cancels anything (issue #2795).
  const hidingProgress = source === 'clone' && cloning;

  const switchSource = (next: MeshSource) => {
    if (next === source) return;
    setSource(next);
    // Drop any failure from the other source so a stale message can't sit
    // above a form it does not describe.
    setFailure(null);
  };

  const handleChooseFolder = async () => {
    setPicking(true);
    setFailure(null);
    try {
      const picked = await pickMeshFolder();
      // null means the user cancelled the OS dialog — leave the prior
      // selection (if any) untouched rather than clearing it.
      if (picked) {
        if (source === 'clone') setParent(picked);
        else setFolder(picked);
      }
    } catch (e) {
      setFailure({ message: formatError(e) });
    } finally {
      setPicking(false);
      // The failure panel may have held the focus, and it is gone by now (the
      // picker was the recovery it offered). The effect above puts the focus
      // back on a control inside the dialog, so the keyboard user is not
      // dropped on the page behind it.
      setRestorePickerFocus(true);
    }
  };

  const handleCreate = async () => {
    if (!folder || creating || createInFlight.current) return;
    // Capture the submitted target up front so the request — and the report
    // of its outcome — is pinned to exactly what the user confirmed.
    const target = folder;
    createInFlight.current = true;
    setCreating(true);
    setFailure(null);
    let result: Awaited<ReturnType<typeof createMesh>>;
    try {
      result = await createMesh(target.name, target.path, color);
    } finally {
      // The guard is released even if the request throws, so a failure can
      // never leave the form permanently unable to submit.
      createInFlight.current = false;
    }
    if (!stillMounted.current) {
      // Dismissed mid-request. The mesh may already exist, so the outcome is
      // reported rather than dropped — the clone flow does the same, because a
      // silent success would leave an unexplained mesh in the sidebar and a
      // silent failure would be lost entirely (issue #2794).
      if ('mesh' in result) {
        addToast('New mesh', `Opened ${target.name}`, 'success');
      } else {
        addToast('New mesh', `Couldn't create the mesh: ${result.error}`, 'error');
      }
      return;
    }
    setCreating(false);
    if ('mesh' in result) {
      enterMesh(result.mesh.id);
      onClose();
      return;
    }
    // The draft survives the failure: the folder and the colour stay on
    // screen, the controls are live again, and the real cause is shown
    // beside the operation with the recovery it supports.
    const classified = classifyMeshCreateFailure(result.error, target.path);
    setFailure({
      message: classified.message,
      hint: classified.hint,
      detail: classified.detail,
      recoveries: classified.recoveries,
    });
  };

  const handleClone = () => {
    if (!parent || !repoName || busy) return;
    // Capture the submitted target up front so the clone is pinned to exactly
    // what the user confirmed — not to a later edit of this form.
    const repo = repoInput.trim();
    const parentPath = parent.path;
    const target = {
      repo,
      parentDir: parentPath,
      destination: joinDisplayPath(parentPath, repoName),
      color,
    };
    setFailure(null);
    // The store owns the operation. When the same clone is already running,
    // this joins THAT operation instead of launching a second one.
    const { operation, joined } = startClone(target);
    setJoinedExisting(joined);
    setCloneOperationId(operation.id);
    // One claim per operation this dialog shows: release the previous one
    // first, so a second submission cannot leave a stale claim behind.
    if (claimedOperationId.current && claimedOperationId.current !== operation.id) {
      releaseOperation(claimedOperationId.current);
    }
    claimedOperationId.current = operation.id;
    claimOperation(operation.id);
  };

  // Act on the operation's outcome once it settles, while this dialog is
  // still open: a success opens the new Mesh, a failure is reported here.
  useEffect(() => {
    if (!cloneOperation || cloneOperation.status === 'running') return;
    if (handledOperationId.current === cloneOperation.id) return;
    handledOperationId.current = cloneOperation.id;
    if (cloneOperation.status === 'succeeded') {
      if (cloneOperation.meshId !== null) enterMesh(cloneOperation.meshId);
      onClose();
    } else {
      // The clone failed. The backend's own message is shown in the failure
      // panel, next to the operation that failed.
      setFailure({ message: cloneOperation.error ?? 'Clone failed. See logs for details.' });
    }
  }, [cloneOperation, onClose]);

  return (
    <Modal
      onClose={onClose}
      labelledBy="mesh-create-title"
      maxWidth="max-w-md"
      closeOnBackdrop={false}
      defaultFocusRef={folderButtonRef}
    >
      <div className="flex items-start justify-between mb-4">
        <h2 id="mesh-create-title" className="text-sm font-semibold text-text-primary">
          New mesh
        </h2>
        {/* While a clone runs, this control hides the progress. It must not
            be labelled "Close" — nothing here stops the clone (issue #2795). */}
        <ModalCloseButton onClose={onClose} label={hidingProgress ? 'Hide progress' : 'Close'} />
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
        <>
          {/* Location */}
          <div className="mb-4">
            <div className={SECTION_LABEL}>Location</div>
            <div className="flex items-center gap-2">
              <button
                type="button"
                ref={folderButtonRef}
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

          {/* The failure sits beside the operation it describes, with the
              recovery it supports. The picked folder and the colour stay on
              screen above it, so the draft survives and the user can act on
              the cause without re-entering anything. */}
          {failure && (
            <MeshCreateFailurePanel
              panelRef={failureRef}
              message={failure.message}
              hint={failure.hint}
              detail={failure.detail}
              onRetry={failure.recoveries?.includes('retry') ? () => void handleCreate() : undefined}
              onChangeFolder={
                failure.recoveries?.includes('change-folder')
                  ? () => void handleChooseFolder()
                  : undefined
              }
            />
          )}
        </>
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
                  handleClone();
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
                ref={folderButtonRef}
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

      {/* The clone flow already reports the backend's own message inline; it
          keeps its place above the action buttons. */}
      {source === 'clone' && failure && (
        <MeshCreateFailurePanel panelRef={failureRef} message={failure.message} />
      )}

      {/* Issue #2795 — the clone has already started and this dialog cannot
          stop it. Say so plainly: dismissing the dialog hides the progress,
          and the operation keeps running where the user can still see it. */}
      {hidingProgress && (
        <>
          {joinedExisting && (
            <p role="status" className="mb-1 text-xs text-text-secondary">
              A clone of this repository into this destination is already running — this joined it.
            </p>
          )}
          <p role="status" className="mb-3 text-xs text-text-secondary">
            The clone continues in the background. Closing this dialog hides the progress — it does
            not stop the clone.
          </p>
        </>
      )}

      <div className="flex justify-end gap-2">
        {/* Before submitting, this button starts no operation and cancels
            nothing — "Cancel" is honest there. Once a clone is running the
            same gesture only hides the progress, so the label changes with
            it. The button stays enabled either way: trapping the user in
            the dialog for the length of a clone would be worse than letting
            them leave an operation they already started. */}
        <button
          type="button"
          onClick={onClose}
          className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
        >
          {hidingProgress ? 'Hide progress' : 'Cancel'}
        </button>
        <button
          type="button"
          onClick={source === 'open' ? handleCreate : handleClone}
          disabled={!canSubmit || busy}
          className="px-3 py-1.5 text-xs font-medium text-text-primary bg-bg-selection hover:bg-bg-card-hover border border-accent-cyan rounded-md transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
        >
          {primaryLabel}
        </button>
      </div>
    </Modal>
  );
}
