/**
 * Provider registry — thin façade over the usagenometer CLI.
 * New CLI providers appear via `usg providers` / `usg json` without JS modules.
 */

import {
    DEFAULT_ENABLED_PROVIDERS,
    PROVIDER_IDS,
    PROVIDER_LABELS,
    createSnapshot,
} from './types.js';
import {
    discoverProviderDefs,
    fetchSnapshotsFromCli,
    resolveCliBinary,
    testProviderFromCli,
} from './cliBackend.js';

export {
    DEFAULT_ENABLED_PROVIDERS,
    PROVIDER_IDS,
    PROVIDER_LABELS,
    resolveCliBinary,
    discoverProviderDefs,
};

export function listProviderDefs() {
    return Object.values(PROVIDER_IDS).map(id => ({
        id,
        label: PROVIDER_LABELS[id],
    }));
}

/**
 * Normalize enabled-provider settings.
 * Accepts known catalog IDs plus any plausible CLI provider id so new
 * Rust-only providers still flow through when listed in gschema.
 * @param {string[]} values
 * @param {string[]} [knownIds]
 * @returns {string[]}
 */
export function normalizeEnabledProviders(values, knownIds = Object.values(PROVIDER_IDS)) {
    const known = knownIds.length > 0 ? knownIds : Object.values(PROVIDER_IDS);
    const knownSet = new Set(known);
    const list = Array.isArray(values) ? values : [];
    const enabled = [];
    const seen = new Set();

    for (const value of list) {
        const id = String(value).trim().toLowerCase();
        if (!id || seen.has(id))
            continue;
        if (knownSet.has(id) || /^[a-z][a-z0-9_-]*$/.test(id)) {
            seen.add(id);
            enabled.push(id);
        }
    }

    // Preserve canonical known order, then extras.
    const ordered = known.filter(id => seen.has(id));
    for (const id of enabled) {
        if (!ordered.includes(id))
            ordered.push(id);
    }
    return ordered;
}

/**
 * Fetch all enabled providers via `usg json`.
 * @param {string[]} enabledIds
 * @returns {Promise<import('./types.js').ProviderSnapshot[]>}
 */
export async function fetchAllProviders(enabledIds) {
    const ids = normalizeEnabledProviders(enabledIds);
    if (ids.length === 0) {
        return Object.values(PROVIDER_IDS).map(id => createSnapshot({
            id,
            label: PROVIDER_LABELS[id],
            status: 'disabled',
            error: 'Disabled',
            meters: [],
        }));
    }
    return fetchSnapshotsFromCli(ids);
}

/**
 * @param {string} providerId
 * @returns {Promise<{ok: boolean, message: string}>}
 */
export async function testProvider(providerId) {
    if (!providerId)
        return {ok: false, message: 'Unknown provider'};
    return testProviderFromCli(providerId);
}
