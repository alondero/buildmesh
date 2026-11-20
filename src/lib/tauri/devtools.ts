import { _invoke } from './_invoke';
import type { DevtoolsStatus } from '../../types/generated/DevtoolsStatus';

/**
 * The DevTools (CDP) port this process opened (issue #2836).
 *
 * The port is a **startup** switch — WebView2 reads its browser arguments
 * when the window is created — so this reports the port that is open *now*,
 * not the port the preference asks for. The two differ whenever the setting
 * changed after the last launch, which is when the UI has to be careful about
 * what it claims.
 */
export const getDevtoolsStatus = () => _invoke<DevtoolsStatus>('get_devtools_status');
