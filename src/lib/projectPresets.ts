export interface ProjectPreset {
  id: string;
  label: string;
  build: string;
  run: string;
}

export interface NodeScripts {
  build: string | null;
  run: string | null;
  has_tauri_cli: boolean;
}

export interface DetectedProject {
  preset_id: string | null;
  label: string | null;
  node_scripts: NodeScripts | null;
}

export const PROJECT_PRESETS: ProjectPreset[] = [
  { id: 'rust',       label: 'Rust (cargo)',         build: 'cargo build',         run: 'cargo run' },
  { id: 'node',       label: 'Node (npm)',           build: 'npm run build',       run: 'npm run dev' },
  { id: 'node-tauri', label: 'Tauri (npm)',          build: 'npm run tauri build', run: 'npm run tauri dev' },
  { id: 'jvm-gradle', label: 'JVM (Gradle wrapper)', build: './gradlew build',     run: './gradlew run' },
  { id: 'jvm-maven',  label: 'JVM (Maven)',          build: 'mvn package',         run: 'mvn spring-boot:run' },
  { id: 'go',         label: 'Go',                   build: 'go build ./...',      run: 'go run .' },
  { id: 'python',     label: 'Python (build)',       build: 'python -m build',     run: 'python -m <module>' },
];

export function findPreset(id: string): ProjectPreset | undefined {
  return PROJECT_PRESETS.find((p) => p.id === id);
}

/**
 * Overlay detected node scripts on a base preset.
 *
 * For node / node-tauri presets, prefer the project's actual `scripts` block
 * (e.g. `serve` when `dev` is absent). When no detection info is given, the
 * base preset is returned unchanged.
 */
export function resolvePreset(
  id: string,
  nodeScripts?: NodeScripts | null
): ProjectPreset | undefined {
  const base = findPreset(id);
  if (!base) return undefined;
  if (id !== 'node' && id !== 'node-tauri') return base;
  if (!nodeScripts) return base;

  const buildScript = nodeScripts.build ?? (id === 'node-tauri' ? 'tauri build' : 'build');
  const runScript = nodeScripts.run ?? (id === 'node-tauri' ? 'tauri dev' : 'dev');

  return {
    ...base,
    build: `npm run ${buildScript}`,
    run: `npm run ${runScript}`,
  };
}
