// Test-only helper for scripts/extension-smoke.sh. GNOME Shell 50 has no
// --unsafe-mode flag, and org.gnome.Shell.Eval answers (false, '') unless
// global.context.unsafe_mode is true. The smoke installs this into its temp
// HOME only; install-extension.sh and pack-extension.sh never ship it.

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

export default class SmokeUnsafe extends Extension {
    enable() {
        global.context.unsafe_mode = true;
    }

    disable() {
        global.context.unsafe_mode = false;
    }
}
