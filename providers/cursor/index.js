import {HttpClient, HttpError} from '../../lib/http.js';
import {
    PROVIDER_IDS,
    PROVIDER_LABELS,
    coerceUnixSeconds,
    createSnapshot,
    meterFromUsedPercent,
} from '../types.js';
import {CursorAuthError, loadCursorAuth} from './auth.js';

const USAGE_SUMMARY_URL = 'https://www.cursor.com/api/usage-summary';

export {CursorAuthError, loadCursorAuth};

export async function fetchCursorSnapshot({httpClient = null} = {}) {
    const client = httpClient ?? new HttpClient();
    const ownsClient = !httpClient;

    try {
        const auth = await loadCursorAuth();
        const summary = await client.getJson(USAGE_SUMMARY_URL, {
            headers: {
                Accept: 'application/json',
                Cookie: auth.sessionCookie,
                'User-Agent': 'Usagenometer/1.0',
            },
        });

        return snapshotFromUsageSummary(summary, auth);
    } catch (error) {
        return createSnapshot({
            id: PROVIDER_IDS.CURSOR,
            label: PROVIDER_LABELS[PROVIDER_IDS.CURSOR],
            status: isAuthError(error) ? 'auth' : 'error',
            error: formatCursorError(error),
            meters: [],
        });
    } finally {
        if (ownsClient)
            client.destroy();
    }
}

export async function testCursorConnection() {
    const snapshot = await fetchCursorSnapshot();
    if (snapshot.status === 'ok') {
        return {
            ok: true,
            message: snapshot.account
                ? `Connected as ${snapshot.account}`
                : `Connected (${snapshot.plan ?? 'Cursor'})`,
            snapshot,
        };
    }
    return {ok: false, message: snapshot.error ?? 'Cursor connection failed'};
}

export function snapshotFromUsageSummary(summary, auth = {}) {
    const individual = summary?.individualUsage ?? {};
    const plan = individual.plan ?? {};
    const onDemand = individual.onDemand ?? {};
    const cycleEnd = coerceUnixSeconds(summary?.billingCycleEnd);

    const meters = [];

    const autoPercent = plan.autoPercentUsed;
    if (autoPercent !== undefined && autoPercent !== null) {
        meters.push(meterFromUsedPercent({
            id: 'auto_composer',
            title: 'Auto + Composer',
            usedPercent: autoPercent,
            resetAt: cycleEnd,
            windowSeconds: null,
        }));
    }

    const apiPercent = plan.apiPercentUsed;
    if (apiPercent !== undefined && apiPercent !== null) {
        meters.push(meterFromUsedPercent({
            id: 'api',
            title: 'API pool',
            usedPercent: apiPercent,
            resetAt: cycleEnd,
            windowSeconds: null,
        }));
    }

    // Fallback messages like "You've used 69% of your included total usage"
    if (meters.length === 0) {
        const autoFromMsg = extractPercent(summary?.autoModelSelectedDisplayMessage);
        const apiFromMsg = extractPercent(summary?.namedModelSelectedDisplayMessage);
        if (autoFromMsg !== null) {
            meters.push(meterFromUsedPercent({
                id: 'auto_composer',
                title: 'Auto + Composer',
                usedPercent: autoFromMsg,
                resetAt: cycleEnd,
            }));
        }
        if (apiFromMsg !== null) {
            meters.push(meterFromUsedPercent({
                id: 'api',
                title: 'API pool',
                usedPercent: apiFromMsg,
                resetAt: cycleEnd,
            }));
        }
    }

    if (onDemand?.enabled && (onDemand.used !== null && onDemand.used !== undefined)) {
        const used = Number(onDemand.used);
        const limit = onDemand.limit !== null && onDemand.limit !== undefined
            ? Number(onDemand.limit)
            : null;
        meters.push({
            id: 'on_demand',
            title: 'On-demand',
            used: Number.isFinite(used) ? used : null,
            left: limit !== null && Number.isFinite(used)
                ? Math.max(limit - used, 0)
                : null,
            limit: Number.isFinite(limit) ? limit : null,
            percent: limit && limit > 0 && Number.isFinite(used) ? used / limit : null,
            leftPercent: limit && limit > 0 && Number.isFinite(used)
                ? Math.max(1 - used / limit, 0)
                : null,
            unit: 'usd',
            resetAt: cycleEnd,
            resetAfterSeconds: null,
            windowSeconds: null,
        });
    }

    return createSnapshot({
        id: PROVIDER_IDS.CURSOR,
        label: PROVIDER_LABELS[PROVIDER_IDS.CURSOR],
        status: 'ok',
        account: auth.email ?? null,
        plan: summary?.membershipType ?? auth.membershipType ?? null,
        meters,
        raw: summary,
    });
}

function extractPercent(message) {
    if (typeof message !== 'string')
        return null;
    const match = message.match(/(\d+(?:\.\d+)?)\s*%/);
    return match ? Number.parseFloat(match[1]) : null;
}

function isAuthError(error) {
    return error instanceof CursorAuthError ||
        (error instanceof HttpError && error.isAuthError);
}

function formatCursorError(error) {
    if (error instanceof CursorAuthError)
        return error.message;
    if (error instanceof HttpError && error.isAuthError)
        return 'Cursor session was rejected. Open Cursor and sign in again.';
    if (error instanceof Error)
        return error.message;
    return 'Unknown Cursor error';
}
