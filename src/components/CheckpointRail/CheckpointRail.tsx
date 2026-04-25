import type { Checkpoint } from '../../stores/sessionStore';
import { revertToCheckpoint } from '../../lib/tauri';

interface CheckpointRailProps {
  checkpoints: Checkpoint[];
}

export function CheckpointRail({ checkpoints }: CheckpointRailProps) {
  const handleRevert = async (checkpointId: number) => {
    if (!confirm('Revert session to this checkpoint? This will discard all changes since.')) {
      return;
    }
    await revertToCheckpoint(checkpointId);
  };

  if (checkpoints.length === 0) {
    return null;
  }

  return (
    <div className="flex items-center gap-2 px-4 py-2 border-b border-[#2a2a2a] bg-[#111] overflow-x-auto">
      <span className="text-xs text-[#666] mr-2">Checkpoints:</span>
      {checkpoints.map((cp, i) => (
        <button
          key={cp.id}
          onClick={() => handleRevert(cp.id)}
          title={cp.message || `Checkpoint ${cp.turn_index}`}
          className="
            flex-shrink-0 w-8 h-8 rounded-full border border-[#2a2a2a]
            bg-[#1a1a1a] text-xs text-[#888]
            hover:border-[#3b82f6] hover:text-[#3b82f6]
            transition-colors cursor-pointer
          "
        >
          {i + 1}
        </button>
      ))}
    </div>
  );
}
