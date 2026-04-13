interface SlashCommandBarProps {
  onCommand: (command: string) => void;
}

const SLASH_COMMANDS = [
  { name: '/auth', description: 'Run auth setup flow' },
  { name: '/api-test', description: 'Test API endpoints' },
  { name: '/deploy-staging', description: 'Deploy to staging' },
  { name: '/fix-lint', description: 'Fix linting errors' },
];

export function SlashCommandBar({ onCommand }: SlashCommandBarProps) {
  if (SLASH_COMMANDS.length === 0) return null;

  return (
    <div className="flex flex-wrap gap-1.5 px-3 py-2 border-t border-[#2a2a2a] bg-[#111]">
      <span className="text-xs text-[#666] mr-1">Quick:</span>
      {SLASH_COMMANDS.map((cmd) => (
        <button
          key={cmd.name}
          onClick={() => onCommand(cmd.name)}
          title={cmd.description}
          className="
            text-xs px-2 py-1 rounded border border-[#2a2a2a]
            bg-[#1a1a1a] text-[#888]
            hover:border-[#3b82f6] hover:text-[#3b82f6]
            transition-colors
          "
        >
          {cmd.name}
        </button>
      ))}
    </div>
  );
}
