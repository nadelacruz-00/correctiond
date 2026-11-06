// Preferences for Correctiond Companion: Extensions (or Extension Manager) >
// Correctiond Companion > Settings. The window itself is in prefsWindow.js.

import {ExtensionPreferences} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

import {fillWindow} from './prefsWindow.js';

export default class CorrectiondPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        window.set_default_size(640, 720);
        fillWindow(window);
    }
}
