import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Gtk from 'gi://Gtk';

import {ExtensionPreferences, gettext as _} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

import {
    DEFAULT_UPDATE_INTERVAL_SECONDS,
    DISPLAY_MODE_LEFT,
    DISPLAY_MODE_USED,
    PANEL_MODE_COMPACT,
    PANEL_MODE_PRIMARY,
} from './constants.js';
import {
    DEFAULT_ENABLED_PROVIDERS,
    PROVIDER_IDS,
    PROVIDER_LABELS,
    listProviderDefs,
    normalizeEnabledProviders,
    testProvider,
} from './providers/registry.js';

const UsagenometerPreferencesPage = GObject.registerClass(
class UsagenometerPreferencesPage extends Adw.PreferencesPage {
    _init(settings) {
        super._init({
            title: _('General'),
            icon_name: 'preferences-system-symbolic',
        });

        this._settings = settings;
        this.add(this._buildGeneralGroup());
        this.add(this._buildProvidersGroup());
        this.add(this._buildConnectionGroup());
    }

    _buildGeneralGroup() {
        const group = new Adw.PreferencesGroup({
            title: _('Refresh & display'),
            description: _('Control how the panel shows AI usage across providers.'),
        });

        const adjustment = new Gtk.Adjustment({
            lower: 60,
            upper: 3600,
            step_increment: 60,
            page_increment: 300,
            value: this._settings.get_int('update-interval-seconds') || DEFAULT_UPDATE_INTERVAL_SECONDS,
        });

        const row = new Adw.SpinRow({
            title: _('Update interval'),
            subtitle: _('Seconds between automatic refreshes'),
            adjustment,
            climb_rate: 1,
            digits: 0,
        });
        this._settings.bind(
            'update-interval-seconds',
            row,
            'value',
            Gio.SettingsBindFlags.DEFAULT,
        );
        group.add(row);

        const displayRow = new Adw.ComboRow({
            title: _('Display value'),
            subtitle: _('Remaining or used quota on meters and the panel.'),
            model: Gtk.StringList.new([_('Left'), _('Used')]),
            selected: this._getDisplayMode() === DISPLAY_MODE_USED ? 1 : 0,
        });
        displayRow.connect('notify::selected', combo => {
            this._settings.set_string(
                'display-mode',
                combo.selected === 1 ? DISPLAY_MODE_USED : DISPLAY_MODE_LEFT,
            );
        });
        group.add(displayRow);

        const panelRow = new Adw.ComboRow({
            title: _('Panel mode'),
            subtitle: _('Compact multi-provider summary or a single primary provider.'),
            model: Gtk.StringList.new([_('Compact all'), _('Primary only')]),
            selected: this._getPanelMode() === PANEL_MODE_PRIMARY ? 1 : 0,
        });
        panelRow.connect('notify::selected', combo => {
            this._settings.set_string(
                'panel-mode',
                combo.selected === 1 ? PANEL_MODE_PRIMARY : PANEL_MODE_COMPACT,
            );
        });
        group.add(panelRow);

        const providerIds = Object.values(PROVIDER_IDS);
        const primaryRow = new Adw.ComboRow({
            title: _('Primary provider'),
            subtitle: _('Used when panel mode is Primary only.'),
            model: Gtk.StringList.new(providerIds.map(id => PROVIDER_LABELS[id])),
            selected: Math.max(0, providerIds.indexOf(this._getPrimaryProvider())),
        });
        primaryRow.connect('notify::selected', combo => {
            const id = providerIds[combo.selected] ?? PROVIDER_IDS.CURSOR;
            this._settings.set_string('primary-provider', id);
        });
        group.add(primaryRow);

        return group;
    }

    _buildProvidersGroup() {
        const group = new Adw.PreferencesGroup({
            title: _('Providers'),
            description: _('Enable the AI usage sources Usagenometer should poll.'),
        });

        const enabled = new Set(this._getEnabledProviders());
        for (const def of listProviderDefs()) {
            const row = new Adw.SwitchRow({
                title: def.label,
                subtitle: providerSubtitle(def.id),
                active: enabled.has(def.id),
            });
            row.connect('notify::active', switchRow => {
                this._setProviderEnabled(def.id, switchRow.active);
            });
            group.add(row);
        }

        return group;
    }

    _buildConnectionGroup() {
        const group = new Adw.PreferencesGroup({
            title: _('Connection tests'),
            description: _('Probe local auth and the provider APIs without changing settings.'),
        });

        for (const def of listProviderDefs()) {
            const row = new Adw.ActionRow({
                title: def.label,
                subtitle: _('Not checked yet.'),
            });
            const button = new Gtk.Button({
                label: _('Test'),
                valign: Gtk.Align.CENTER,
            });
            button.connect('clicked', () => {
                void this._runProviderTest(def.id, row);
            });
            row.add_suffix(button);
            group.add(row);
        }

        return group;
    }

    async _runProviderTest(providerId, row) {
        row.subtitle = _('Testing...');
        try {
            const result = await testProvider(providerId);
            row.subtitle = result.message;
        } catch (error) {
            row.subtitle = error instanceof Error ? error.message : String(error);
        }
    }

    _getDisplayMode() {
        const mode = this._settings.get_string('display-mode');
        return mode === DISPLAY_MODE_USED ? DISPLAY_MODE_USED : DISPLAY_MODE_LEFT;
    }

    _getPanelMode() {
        const mode = this._settings.get_string('panel-mode');
        return mode === PANEL_MODE_PRIMARY ? PANEL_MODE_PRIMARY : PANEL_MODE_COMPACT;
    }

    _getPrimaryProvider() {
        const value = this._settings.get_string('primary-provider');
        return Object.values(PROVIDER_IDS).includes(value) ? value : PROVIDER_IDS.CURSOR;
    }

    _getEnabledProviders() {
        try {
            const values = this._settings.get_strv('enabled-providers');
            const normalized = normalizeEnabledProviders(values);
            return normalized.length > 0 ? normalized : [...DEFAULT_ENABLED_PROVIDERS];
        } catch (error) {
            return [...DEFAULT_ENABLED_PROVIDERS];
        }
    }

    _setProviderEnabled(providerId, enabled) {
        const current = new Set(this._getEnabledProviders());
        if (enabled)
            current.add(providerId);
        else
            current.delete(providerId);

        const ordered = Object.values(PROVIDER_IDS).filter(id => current.has(id));
        this._settings.set_strv(
            'enabled-providers',
            ordered.length > 0 ? ordered : [...DEFAULT_ENABLED_PROVIDERS],
        );
    }
});

export default class UsagenometerPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();
        window.add(new UsagenometerPreferencesPage(settings));
    }
}

function providerSubtitle(id) {
    switch (id) {
    case PROVIDER_IDS.CODEX:
        return _('Local Codex CLI auth (~/.codex/auth.json)');
    case PROVIDER_IDS.CURSOR:
        return _('Cursor app session (state.vscdb) · Auto+Composer and API pools');
    case PROVIDER_IDS.ANTIGRAVITY:
        return _('Antigravity secret-store OAuth · Gemini and Claude/GPT quotas');
    case PROVIDER_IDS.CLAUDE:
        return _('Claude CLI detected · quota details are available inside Claude with /usage');
    case PROVIDER_IDS.GROK:
        return _('Grok CLI detected · no local quota endpoint is exposed');
    default:
        return '';
    }
}
