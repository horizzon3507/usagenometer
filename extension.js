import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';

import {Extension, gettext as _} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

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
    fetchAllProviders,
    normalizeEnabledProviders,
} from './providers/registry.js';

const PROGRESS_BAR_WIDTH = 380;
const PROGRESS_BAR_HEIGHT = 6;
const PANEL_ICON_SIZE = 16;
const MENU_TITLE_STYLE = 'color: #fff;';
const PROVIDER_SHORT = {
    [PROVIDER_IDS.CODEX]: 'X',
    [PROVIDER_IDS.CURSOR]: 'C',
    [PROVIDER_IDS.ANTIGRAVITY]: 'A',
    [PROVIDER_IDS.CLAUDE]: 'Cl',
    [PROVIDER_IDS.GROK]: 'G',
};

const UsagenometerIndicator = GObject.registerClass(
class UsagenometerIndicator extends PanelMenu.Button {
    _init(extension) {
        super._init(0.5, _('Usagenometer (beta)'));

        this._extension = extension;
        this._settings = extension.getSettings();
        this._menuOpenStateChangedId = null;
        this._refreshSourceId = null;
        this._refreshInFlight = null;
        this._state = {
            providers: [],
            lastUpdated: null,
        };

        const box = new St.BoxLayout({
            style_class: 'panel-status-menu-box',
        });
        this._icon = new St.Icon({
            gicon: Gio.icon_new_for_string(GLib.build_filenamev([
                this._extension.path,
                'icons',
                'usagenometer-symbolic.svg',
            ])),
            icon_size: PANEL_ICON_SIZE,
            style_class: 'system-status-icon',
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._label = new St.Label({
            text: _('--'),
            y_align: Clutter.ActorAlign.CENTER,
        });
        box.add_child(this._icon);
        box.add_child(this._label);
        this.add_child(box);

        this._buildMenu();
        this._menuOpenStateChangedId = this.menu.connect('open-state-changed', (_menu, isOpen) => {
            if (isOpen)
                void this.refresh();
        });

        this._settings.connectObject(
            'changed::update-interval-seconds',
            () => this._restartRefreshTimer(),
            this,
        );
        this._settings.connectObject(
            'changed::display-mode',
            () => this._renderCurrentState(),
            this,
        );
        this._settings.connectObject(
            'changed::enabled-providers',
            () => void this.refresh(),
            this,
        );
        this._settings.connectObject(
            'changed::panel-mode',
            () => this._renderCurrentState(),
            this,
        );
        this._settings.connectObject(
            'changed::primary-provider',
            () => this._renderCurrentState(),
            this,
        );

        this._restartRefreshTimer();
        this._renderCurrentState();
        void this.refresh();
    }

    _buildMenu() {
        this._usageSection = new PopupMenu.PopupMenuSection();
        this.menu.addMenuItem(this._usageSection);

        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        this._refreshItem = new PopupMenu.PopupBaseMenuItem();
        this._refreshItem.add_child(new St.Label({
            text: _('Refresh now'),
            x_expand: true,
            x_align: Clutter.ActorAlign.START,
        }));
        this._refreshTimestampLabel = new St.Label({
            text: formatLastUpdatedValue(this._state),
            style_class: 'dim-label',
            x_align: Clutter.ActorAlign.END,
        });
        this._refreshItem.add_child(this._refreshTimestampLabel);
        this._refreshItem.connect('activate', () => {
            void this.refresh();
        });
        this.menu.addMenuItem(this._refreshItem);

        this.menu.addAction(_('Settings'), () => {
            this._extension.openPreferences();
        });
    }

    async refresh() {
        if (this._refreshInFlight)
            return this._refreshInFlight;

        this._refreshTimestampLabel.text = _('Refreshing...');
        this._refreshInFlight = this._refreshUsage()
            .catch(error => {
                reportError(error, '[usagenometer] refresh failed');
            })
            .finally(() => {
                this._refreshInFlight = null;
                try {
                    this._renderCurrentState();
                } catch (error) {
                    reportError(error, '[usagenometer] render failed');
                }
            });

        return this._refreshInFlight;
    }

    async _refreshUsage() {
        try {
            const enabled = this._getEnabledProviders();
            const providers = await fetchAllProviders(enabled);
            this._state = {
                providers,
                lastUpdated: GLib.DateTime.new_now_local(),
            };
        } catch (error) {
            reportError(error, '[usagenometer] usage refresh failed');
            this._state = {
                ...this._state,
                providers: this._state.providers.map(provider => ({
                    ...provider,
                    status: provider.status === 'ok' ? 'ok' : 'error',
                    error: provider.error ?? formatGenericError(error),
                })),
            };
        }
    }

    _renderCurrentState() {
        const displayMode = this._getDisplayMode();
        this._setLabel(formatPanelLabel(this._state, displayMode, {
            panelMode: this._getPanelMode(),
            primaryProvider: this._getPrimaryProvider(),
        }));
        this._refreshTimestampLabel.text = formatLastUpdatedValue(this._state);
        this._renderUsage(this._state, displayMode);
    }

    _renderUsage(state, displayMode) {
        this._usageSection.removeAll();

        const providers = state.providers ?? [];
        if (providers.length === 0) {
            this._usageSection.addMenuItem(new PopupMenu.PopupMenuItem(
                _('No providers enabled.'),
                {reactive: false, can_focus: false},
            ));
            return;
        }

        for (const provider of providers)
            this._usageSection.addMenuItem(createProviderMenuItem(provider, displayMode));
    }

    _restartRefreshTimer() {
        if (this._refreshSourceId) {
            GLib.Source.remove(this._refreshSourceId);
            this._refreshSourceId = null;
        }

        const interval = Math.max(
            60,
            this._settings.get_int('update-interval-seconds') || DEFAULT_UPDATE_INTERVAL_SECONDS,
        );

        this._refreshSourceId = GLib.timeout_add_seconds(
            GLib.PRIORITY_DEFAULT,
            interval,
            () => {
                void this.refresh();
                return GLib.SOURCE_CONTINUE;
            },
        );
    }

    _setLabel(text) {
        this._label.text = text;
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

    destroy() {
        if (this._refreshSourceId) {
            GLib.Source.remove(this._refreshSourceId);
            this._refreshSourceId = null;
        }

        this._settings.disconnectObject(this);
        if (this._menuOpenStateChangedId) {
            this.menu.disconnect(this._menuOpenStateChangedId);
            this._menuOpenStateChangedId = null;
        }
        super.destroy();
    }
});

export default class UsagenometerExtension extends Extension {
    enable() {
        this._indicator = new UsagenometerIndicator(this);
        Main.panel.addToStatusArea(this.uuid, this._indicator, 0, 'right');
    }

    disable() {
        this._indicator?.destroy();
        this._indicator = null;
    }
}

function reportError(error, context) {
    if (typeof globalThis.logError === 'function') {
        globalThis.logError(error, context);
        return;
    }

    const detail = error instanceof Error
        ? error.stack ?? error.message
        : String(error);
    console.error(`${context}: ${detail}`);
}

function createInfoMenuItem(title, subtitle = '', meta = '') {
    const menuItem = new PopupMenu.PopupBaseMenuItem({
        reactive: false,
        can_focus: false,
    });

    const content = new St.BoxLayout({
        vertical: true,
        x_expand: true,
    });
    content.add_child(new St.Label({
        text: title,
        style: MENU_TITLE_STYLE,
        x_align: Clutter.ActorAlign.START,
    }));

    if (subtitle) {
        content.add_child(new St.Label({
            text: subtitle,
            style_class: 'dim-label',
            x_align: Clutter.ActorAlign.START,
        }));
    }

    if (meta) {
        content.add_child(new St.Label({
            text: meta,
            style_class: 'dim-label',
            x_align: Clutter.ActorAlign.START,
        }));
    }

    menuItem.add_child(content);
    return menuItem;
}

function createProviderMenuItem(provider, displayMode) {
    const menuItem = new PopupMenu.PopupBaseMenuItem({
        reactive: false,
        can_focus: false,
        style_class: 'usagenometer-provider',
    });
    const content = new St.BoxLayout({vertical: true, x_expand: true});
    const header = new St.BoxLayout({x_expand: true});
    header.add_child(new St.Label({
        text: formatProviderTitle(provider),
        style: 'font-weight: 700;',
        x_expand: true,
        x_align: Clutter.ActorAlign.START,
    }));
    header.add_child(new St.Label({
        text: provider.status === 'ok' ? formatProviderMeta(provider) : provider.status,
        style_class: 'dim-label',
        x_align: Clutter.ActorAlign.END,
    }));
    content.add_child(header);

    if (provider.account) {
        content.add_child(new St.Label({
            text: provider.account,
            style_class: 'dim-label',
            x_align: Clutter.ActorAlign.START,
        }));
    }

    if (provider.status !== 'ok' || provider.meters.length === 0) {
        content.add_child(new St.Label({
            text: provider.error ?? _('No usage data available.'),
            style_class: 'dim-label',
            x_align: Clutter.ActorAlign.START,
        }));
    } else {
        for (const meter of provider.meters)
            content.add_child(createCompactMeter(meter, displayMode));
    }

    menuItem.add_child(content);
    return menuItem;
}

function createCompactMeter(meter, displayMode) {
    const row = new St.BoxLayout({vertical: true, x_expand: true});
    const labels = new St.BoxLayout({x_expand: true});
    labels.add_child(new St.Label({
        text: meter.title,
        style_class: 'dim-label',
        x_expand: true,
        x_align: Clutter.ActorAlign.START,
    }));
    labels.add_child(new St.Label({
        text: formatMeterValue(meter, displayMode),
        style: 'font-weight: 700;',
        x_align: Clutter.ActorAlign.END,
    }));
    row.add_child(labels);
    row.add_child(createProgressBar(getMeterProgressPercent(meter, displayMode), displayMode));
    return row;
}

function createUsageProgressMenuItem(title, meter, displayMode) {
    const menuItem = new PopupMenu.PopupBaseMenuItem({
        reactive: false,
        can_focus: false,
    });

    const content = new St.BoxLayout({
        vertical: true,
        x_expand: true,
    });

    content.add_child(new St.Label({
        text: title,
        style: MENU_TITLE_STYLE,
        x_align: Clutter.ActorAlign.START,
    }));

    content.add_child(new St.Label({
        text: formatMeterValue(meter, displayMode),
        style: 'font-weight: 700; font-size: 1.08em;',
        x_align: Clutter.ActorAlign.START,
    }));

    content.add_child(createProgressBar(getMeterProgressPercent(meter, displayMode), displayMode));

    const subtitle = formatMeterSubtitle(meter);
    if (subtitle) {
        content.add_child(new St.Label({
            text: subtitle,
            style_class: 'dim-label',
            x_align: Clutter.ActorAlign.START,
        }));
    }

    menuItem.add_child(content);
    return menuItem;
}

function createProgressBar(percent, displayMode) {
    const normalized = normalizeProgressPercent(percent);
    const fillWidth = normalized === null
        ? 0
        : Math.round(PROGRESS_BAR_WIDTH * normalized);
    const fill = fillWidth > 0
        ? new St.Widget({
            width: fillWidth,
            height: PROGRESS_BAR_HEIGHT,
            style: [
                `background-color: ${getProgressColor(normalized, displayMode)};`,
                `border-radius: ${Math.floor(PROGRESS_BAR_HEIGHT / 2)}px;`,
            ].join(' '),
        })
        : null;

    const track = new St.Widget({
        width: PROGRESS_BAR_WIDTH,
        height: PROGRESS_BAR_HEIGHT,
        x_align: Clutter.ActorAlign.START,
        layout_manager: new Clutter.FixedLayout(),
        style: [
            'background-color: rgba(255, 255, 255, 0.16);',
            `border-radius: ${Math.floor(PROGRESS_BAR_HEIGHT / 2)}px;`,
            'margin-top: 3px;',
            'margin-bottom: 4px;',
        ].join(' '),
    });

    if (fill) {
        fill.set_position(0, 0);
        track.add_child(fill);
    }

    return track;
}

function formatPanelLabel(state, displayMode, {panelMode, primaryProvider}) {
    const providers = state.providers ?? [];
    if (providers.length === 0)
        return _('--');

    if (panelMode === PANEL_MODE_PRIMARY) {
        const primary = providers.find(provider => provider.id === primaryProvider) ??
            providers[0];
        return formatSingleProviderPanel(primary, displayMode);
    }

    const parts = providers.map(provider => {
        const short = PROVIDER_SHORT[provider.id] ?? provider.id.slice(0, 1).toUpperCase();
        if (provider.status !== 'ok')
            return `${short}!`;
        const meter = pickPrimaryMeter(provider);
        if (!meter)
            return `${short}–`;
        return `${short} ${formatMeterCompact(meter, displayMode)}`;
    });

    return parts.join(' · ') || _('--');
}

function formatSingleProviderPanel(provider, displayMode) {
    if (!provider)
        return _('--');
    if (provider.status === 'auth' || provider.status === 'error')
        return _('!');
    if (provider.status !== 'ok')
        return _('--');

    const meter = pickPrimaryMeter(provider);
    if (!meter)
        return _('--');
    return formatMeterCompact(meter, displayMode);
}

function pickPrimaryMeter(provider) {
    if (!provider?.meters?.length)
        return null;
    // Prefer API/5h style meters first for "most actionable"
    const preferred = provider.meters.find(meter =>
        /api|5h|session|primary/i.test(`${meter.id} ${meter.title}`));
    return preferred ?? provider.meters[0];
}

function formatMeterCompact(meter, displayMode) {
    if (displayMode === DISPLAY_MODE_USED) {
        if (meter.percent !== null)
            return `${Math.round(meter.percent * 100)}%`;
        if (meter.used !== null)
            return formatCompact(meter.used);
    } else {
        if (meter.leftPercent !== null)
            return `${Math.round(meter.leftPercent * 100)}%`;
        if (meter.left !== null)
            return formatCompact(meter.left);
    }
    return 'n/a';
}

function formatProviderTitle(provider) {
    const label = provider.label ?? PROVIDER_LABELS[provider.id] ?? provider.id;
    if (provider.plan)
        return `${label} · ${formatPlanType(provider.plan)}`;
    return label;
}

function formatProviderSummary(provider, displayMode) {
    if (provider.status !== 'ok')
        return provider.error ?? _('Unavailable');

    if (provider.meters.length === 0)
        return _('No meters returned');

    const parts = [];
    if (provider.plan)
        parts.push(formatPlanType(provider.plan));

    const worst = pickWorstMeter(provider.meters, displayMode);
    if (worst)
        parts.push(`${worst.title}: ${formatMeterCompact(worst, displayMode)}`);

    return parts.join(' · ') || _('OK');
}

function formatProviderMeta(provider) {
    if (provider.status === 'ok')
        return `${provider.meters.length} ${_('meters')}`;
    return provider.status;
}

function pickWorstMeter(meters, displayMode) {
    let best = null;
    let bestScore = -1;
    for (const meter of meters) {
        const used = displayMode === DISPLAY_MODE_USED
            ? (meter.percent ?? (meter.leftPercent !== null ? 1 - meter.leftPercent : null))
            : (meter.leftPercent !== null ? 1 - meter.leftPercent : meter.percent);
        if (used === null)
            continue;
        if (used > bestScore) {
            bestScore = used;
            best = meter;
        }
    }
    return best ?? meters[0] ?? null;
}

function formatMeterValue(meter, displayMode) {
    if (displayMode === DISPLAY_MODE_USED) {
        if (meter.unit === 'percent' && meter.percent !== null)
            return `${Math.round(meter.percent * 100)}% used`;
        if (meter.used !== null && meter.limit !== null)
            return `${formatNumber(meter.used)} / ${formatNumber(meter.limit)} used`;
        if (meter.used !== null)
            return `${formatNumber(meter.used)} used`;
        if (meter.percent !== null)
            return `${Math.round(meter.percent * 100)}% used`;
    } else {
        if (meter.unit === 'percent' && meter.leftPercent !== null)
            return `${Math.round(meter.leftPercent * 100)}% remaining`;
        if (meter.left !== null && meter.limit !== null)
            return `${formatNumber(meter.left)} left of ${formatNumber(meter.limit)}`;
        if (meter.left !== null)
            return `${formatNumber(meter.left)} remaining`;
        if (meter.leftPercent !== null)
            return `${Math.round(meter.leftPercent * 100)}% remaining`;
    }
    return _('Unavailable');
}

function formatMeterSubtitle(meter) {
    const parts = [];
    const resetText = formatMeterReset(meter);
    if (resetText)
        parts.push(resetText);
    if (meter.limit !== null && meter.unit !== 'percent')
        parts.push(`${formatNumber(meter.limit)} total`);
    return parts.join('  •  ');
}

function formatMeterReset(meter) {
    if (typeof meter.resetAt === 'number' && Number.isFinite(meter.resetAt)) {
        const resetDateTime = GLib.DateTime.new_from_unix_local(Math.round(meter.resetAt));
        const now = GLib.DateTime.new_now_local();
        if (resetDateTime && now && isSameDay(resetDateTime, now))
            return `Resets ${resetDateTime.format('%H:%M')}`;
        if (resetDateTime)
            return `Resets ${resetDateTime.format('%b %d, %Y %H:%M')}`;
    }

    if (typeof meter.resetAfterSeconds === 'number' && Number.isFinite(meter.resetAfterSeconds))
        return `Resets in ${formatDuration(meter.resetAfterSeconds)}`;

    return '';
}

function getMeterProgressPercent(meter, displayMode) {
    if (displayMode === DISPLAY_MODE_USED)
        return meter.percent ?? (meter.leftPercent !== null ? 1 - meter.leftPercent : null);
    return meter.leftPercent ?? (meter.percent !== null ? 1 - meter.percent : null);
}

function normalizeProgressPercent(percent) {
    if (typeof percent !== 'number' || !Number.isFinite(percent))
        return null;
    return Math.max(0, Math.min(percent, 1));
}

function getProgressColor(percent, displayMode) {
    if (displayMode !== DISPLAY_MODE_USED) {
        if (percent <= 0.1)
            return '#ed333b';
        if (percent <= 0.3)
            return '#f6d32d';
        return '#2ec27e';
    }

    if (percent >= 0.9)
        return '#ed333b';
    if (percent >= 0.7)
        return '#f6d32d';
    return '#62a0ea';
}

function formatLastUpdatedValue(state) {
    if (!state.lastUpdated)
        return _('never');
    return state.lastUpdated.format('%F %R');
}

function formatPlanType(planType) {
    const normalized = String(planType).trim();
    if (!normalized)
        return '';

    const knownNames = {
        free: 'Free',
        go: 'Go',
        plus: 'Plus',
        pro: 'Pro',
        team: 'Team',
        enterprise: 'Enterprise',
        edu: 'Edu',
        business: 'Business',
        prolite: 'Pro Lite',
    };

    return knownNames[normalized.toLowerCase()]
        ?? normalized
            .replace(/[_-]+/g, ' ')
            .replace(/\b\w/g, char => char.toUpperCase());
}

function formatGenericError(error) {
    if (error instanceof Error)
        return error.message;
    return _('Unknown error');
}

function isSameDay(left, right) {
    return left.get_year() === right.get_year() &&
        left.get_month() === right.get_month() &&
        left.get_day_of_month() === right.get_day_of_month();
}

function formatNumber(value) {
    return new Intl.NumberFormat().format(Math.round(value));
}

function formatCompact(value) {
    return new Intl.NumberFormat(undefined, {
        notation: 'compact',
        maximumFractionDigits: 1,
    }).format(value);
}

function formatDuration(totalSeconds) {
    const seconds = Math.max(0, Math.round(totalSeconds));
    const hours = Math.floor(seconds / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);

    if (hours > 0 && minutes > 0)
        return `${hours}h ${minutes}m`;
    if (hours > 0)
        return `${hours}h`;
    if (minutes > 0)
        return `${minutes}m`;
    return `${seconds}s`;
}
