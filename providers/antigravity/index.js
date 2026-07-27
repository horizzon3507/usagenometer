import {HttpClient, HttpError} from '../../lib/http.js';
import {
    PROVIDER_IDS,
    PROVIDER_LABELS,
    coerceUnixSeconds,
    createSnapshot,
    meterFromRemainingFraction,
} from '../types.js';
import {AntigravityAuthError, loadAntigravityAuth} from './auth.js';

const CLOUD_CODE_BASES = [
    'https://daily-cloudcode-pa.googleapis.com',
    'https://cloudcode-pa.googleapis.com',
];
const QUOTA_SUMMARY_PATH = '/v1internal:retrieveUserQuotaSummary';
const USER_AGENT = 'antigravity/usagenometer';

const SUMMARY_BUCKETS = [
    {bucketId: 'gemini-5h', title: 'Gemini 5h', windowSeconds: 5 * 3600},
    {bucketId: 'gemini-weekly', title: 'Gemini weekly', windowSeconds: 7 * 86400},
    {bucketId: '3p-5h', title: 'Claude/GPT 5h', windowSeconds: 5 * 3600},
    {bucketId: '3p-weekly', title: 'Claude/GPT weekly', windowSeconds: 7 * 86400},
];

export {AntigravityAuthError, loadAntigravityAuth};

export async function fetchAntigravitySnapshot({httpClient = null} = {}) {
    const client = httpClient ?? new HttpClient();
    const ownsClient = !httpClient;

    try {
        const auth = await loadAntigravityAuth({httpClient: client});
        const summary = await fetchQuotaSummary(client, auth.accessToken);
        return snapshotFromQuotaSummary(summary, auth);
    } catch (error) {
        return createSnapshot({
            id: PROVIDER_IDS.ANTIGRAVITY,
            label: PROVIDER_LABELS[PROVIDER_IDS.ANTIGRAVITY],
            status: isAuthError(error) ? 'auth' : 'error',
            error: formatAntigravityError(error),
            meters: [],
        });
    } finally {
        if (ownsClient)
            client.destroy();
    }
}

export async function testAntigravityConnection() {
    const snapshot = await fetchAntigravitySnapshot();
    if (snapshot.status === 'ok') {
        return {
            ok: true,
            message: snapshot.account
                ? `Connected as ${snapshot.account}`
                : `Connected · ${snapshot.meters.length} quota pools`,
            snapshot,
        };
    }
    return {ok: false, message: snapshot.error ?? 'Antigravity connection failed'};
}

async function fetchQuotaSummary(client, accessToken) {
    let lastError = null;
    for (const base of CLOUD_CODE_BASES) {
        try {
            return await client.postJson(`${base}${QUOTA_SUMMARY_PATH}`, {}, {
                headers: {
                    Accept: 'application/json',
                    Authorization: `Bearer ${accessToken}`,
                    'User-Agent': USER_AGENT,
                },
            });
        } catch (error) {
            lastError = error;
            if (error instanceof HttpError && error.isAuthError)
                throw error;
        }
    }
    throw lastError ?? new Error('Antigravity quota summary unavailable.');
}

export function snapshotFromQuotaSummary(summary, auth = {}) {
    const groups = summary?.response?.groups ?? summary?.groups ?? [];
    const byId = new Map();

    for (const group of groups) {
        for (const bucket of group?.buckets ?? []) {
            const id = bucket?.bucketId;
            if (!id || byId.has(id))
                continue;
            byId.set(id, bucket);
        }
    }

    const meters = [];
    for (const spec of SUMMARY_BUCKETS) {
        const bucket = byId.get(spec.bucketId);
        if (!bucket)
            continue;
        const remaining = bucket.remainingFraction;
        if (typeof remaining !== 'number' || !Number.isFinite(remaining))
            continue;

        meters.push(meterFromRemainingFraction({
            id: spec.bucketId,
            title: bucket.displayName
                ? `${groupDisplayPrefix(spec.bucketId)}${bucket.displayName}`
                : spec.title,
            remainingFraction: remaining,
            resetAt: coerceUnixSeconds(bucket.resetTime),
            windowSeconds: spec.windowSeconds,
        }));
    }

    return createSnapshot({
        id: PROVIDER_IDS.ANTIGRAVITY,
        label: PROVIDER_LABELS[PROVIDER_IDS.ANTIGRAVITY],
        status: 'ok',
        account: auth.account ?? null,
        plan: null,
        meters,
        raw: summary,
    });
}

function groupDisplayPrefix(bucketId) {
    if (bucketId.startsWith('gemini-'))
        return 'Gemini · ';
    if (bucketId.startsWith('3p-'))
        return 'Claude/GPT · ';
    return '';
}

function isAuthError(error) {
    return error instanceof AntigravityAuthError ||
        (error instanceof HttpError && error.isAuthError);
}

function formatAntigravityError(error) {
    if (error instanceof AntigravityAuthError)
        return error.message;
    if (error instanceof HttpError && error.isAuthError)
        return 'Antigravity token was rejected. Sign in again with Antigravity.';
    if (error instanceof Error)
        return error.message;
    return 'Unknown Antigravity error';
}
