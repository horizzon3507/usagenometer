import {CodexCliAuthError, loadCodexCliAuth} from '../../codexAuth.js';
import {UsageApiClient, UsageApiError} from '../../usageApi.js';
import {
    PROVIDER_IDS,
    PROVIDER_LABELS,
    coerceNumber,
    createMeter,
    createSnapshot,
} from '../types.js';

export {CodexCliAuthError, loadCodexCliAuth, UsageApiClient, UsageApiError};

export async function fetchCodexSnapshot({httpClient = null} = {}) {
    const client = httpClient ?? new UsageApiClient();
    const ownsClient = !httpClient;

    try {
        const auth = await loadCodexCliAuth();
        const summary = await client.fetchSummary(auth.accessToken);
        return createSnapshot({
            id: PROVIDER_IDS.CODEX,
            label: PROVIDER_LABELS[PROVIDER_IDS.CODEX],
            status: 'ok',
            account: summary.email ?? null,
            plan: summary.planType ?? null,
            meters: metersFromCodexSummary(summary),
            raw: summary,
        });
    } catch (error) {
        const status = isAuthError(error) ? 'auth' : 'error';
        return createSnapshot({
            id: PROVIDER_IDS.CODEX,
            label: PROVIDER_LABELS[PROVIDER_IDS.CODEX],
            status,
            error: formatCodexError(error),
            meters: [],
        });
    } finally {
        if (ownsClient)
            client.destroy();
    }
}

export async function testCodexConnection() {
    const client = new UsageApiClient();
    try {
        const auth = await loadCodexCliAuth();
        const summary = await client.fetchSummary(auth.accessToken);
        return {
            ok: true,
            message: summary.email
                ? `Connected as ${summary.email}`
                : 'Connection OK',
            snapshot: createSnapshot({
                id: PROVIDER_IDS.CODEX,
                label: PROVIDER_LABELS[PROVIDER_IDS.CODEX],
                status: 'ok',
                account: summary.email ?? null,
                plan: summary.planType ?? null,
                meters: metersFromCodexSummary(summary),
                raw: summary,
            }),
        };
    } catch (error) {
        return {
            ok: false,
            message: formatCodexError(error),
        };
    } finally {
        client.destroy();
    }
}

function metersFromCodexSummary(summary) {
    const meters = [];
    const seen = new Set();

    const pushWindow = (window, fallbackTitle) => {
        if (!window)
            return;
        const rawId = String(window.id ?? window.label ?? fallbackTitle);
        // The API may expose primary/week windows both as named fields and in
        // `windows`. Collapse those aliases so the popup never shows duplicates.
        const id = /week|7.?day/i.test(rawId)
            ? 'weekly'
            : /5.?h|hour|primary|session/i.test(rawId)
                ? 'primary'
                : rawId;
        if (seen.has(id))
            return;
        seen.add(id);

        meters.push(createMeter({
            id: String(id),
            title: window.title ?? window.label ?? fallbackTitle,
            used: window.used,
            left: window.left,
            limit: window.limit,
            percent: window.percent,
            leftPercent: window.leftPercent,
            unit: 'percent',
            resetAt: window.resetAt,
            resetAfterSeconds: window.resetAfterSeconds,
            windowSeconds: window.windowSeconds,
        }));
    };

    if (summary.primaryWindow)
        pushWindow({...summary.primaryWindow, title: '5 hour usage limit', id: 'primary'}, '5h');
    if (summary.weekWindow)
        pushWindow({...summary.weekWindow, title: 'Weekly usage limit', id: 'weekly'}, 'week');

    for (const window of summary.windows ?? []) {
        const title = window.label ?? window.id ?? 'Usage';
        pushWindow({...window, title}, title);
    }

    // Fallback: single aggregate meter from summary totals
    if (meters.length === 0 && (summary.percent !== null || summary.used !== null)) {
        meters.push(createMeter({
            id: 'summary',
            title: 'Usage',
            used: summary.used,
            left: summary.left,
            limit: summary.limit,
            percent: summary.percent,
            leftPercent: summary.leftPercent,
            unit: 'percent',
            resetAt: summary.resetAt,
            resetAfterSeconds: summary.resetAfterSeconds,
        }));
    }

    return meters;
}

function isAuthError(error) {
    return error instanceof CodexCliAuthError ||
        (error instanceof UsageApiError && error.isAuthError);
}

function formatCodexError(error) {
    if (error instanceof CodexCliAuthError)
        return error.message;
    if (error instanceof UsageApiError && error.isAuthError)
        return 'Codex CLI token was rejected. Run codex login.';
    if (error instanceof Error)
        return error.message;
    return 'Unknown Codex error';
}

// silence unused import lint-style
void coerceNumber;
