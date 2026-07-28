import {fetchAntigravitySnapshot, testAntigravityConnection} from './antigravity/index.js';
import {fetchCodexSnapshot, testCodexConnection} from './codex/index.js';
import {fetchCursorSnapshot, testCursorConnection} from './cursor/index.js';
import {
    fetchClaudeSnapshot,
    fetchGrokSnapshot,
    testClaudeConnection,
    testGrokConnection,
} from './cli/index.js';
import {
    DEFAULT_ENABLED_PROVIDERS,
    PROVIDER_IDS,
    PROVIDER_LABELS,
    createSnapshot,
} from './types.js';

const FETCHERS = {
    [PROVIDER_IDS.CODEX]: fetchCodexSnapshot,
    [PROVIDER_IDS.CURSOR]: fetchCursorSnapshot,
    [PROVIDER_IDS.ANTIGRAVITY]: fetchAntigravitySnapshot,
    [PROVIDER_IDS.CLAUDE]: fetchClaudeSnapshot,
    [PROVIDER_IDS.GROK]: fetchGrokSnapshot,
};

const TESTERS = {
    [PROVIDER_IDS.CODEX]: testCodexConnection,
    [PROVIDER_IDS.CURSOR]: testCursorConnection,
    [PROVIDER_IDS.ANTIGRAVITY]: testAntigravityConnection,
    [PROVIDER_IDS.CLAUDE]: testClaudeConnection,
    [PROVIDER_IDS.GROK]: testGrokConnection,
};

export {
    DEFAULT_ENABLED_PROVIDERS,
    PROVIDER_IDS,
    PROVIDER_LABELS,
};

export function listProviderDefs() {
    return Object.values(PROVIDER_IDS).map(id => ({
        id,
        label: PROVIDER_LABELS[id],
    }));
}

export function normalizeEnabledProviders(values) {
    const allowed = new Set(Object.values(PROVIDER_IDS));
    const list = Array.isArray(values) ? values : [];
    const enabled = list
        .map(value => String(value).trim())
        .filter(value => allowed.has(value));

    // Preserve canonical order
    return Object.values(PROVIDER_IDS).filter(id => enabled.includes(id));
}

/**
 * Fetch all enabled providers in parallel.
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

    const results = await Promise.all(ids.map(async id => {
        const fetch = FETCHERS[id];
        try {
            return await fetch();
        } catch (error) {
            return createSnapshot({
                id,
                label: PROVIDER_LABELS[id],
                status: 'error',
                error: error instanceof Error ? error.message : String(error),
                meters: [],
            });
        }
    }));

    return results;
}

export async function testProvider(providerId) {
    const tester = TESTERS[providerId];
    if (!tester)
        return {ok: false, message: `Unknown provider: ${providerId}`};
    return tester();
}
