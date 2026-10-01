export const PROVIDER_IDS = Object.freeze({
    CODEX: 'codex',
    CURSOR: 'cursor',
    ANTIGRAVITY: 'antigravity',
    CLAUDE: 'claude',
    GROK: 'grok',
});

export const PROVIDER_LABELS = Object.freeze({
    [PROVIDER_IDS.CODEX]: 'Codex',
    [PROVIDER_IDS.CURSOR]: 'Cursor',
    [PROVIDER_IDS.ANTIGRAVITY]: 'Antigravity',
    [PROVIDER_IDS.CLAUDE]: 'Claude',
    [PROVIDER_IDS.GROK]: 'Grok',
});

export const DEFAULT_ENABLED_PROVIDERS = Object.freeze([
    PROVIDER_IDS.CODEX,
    PROVIDER_IDS.CURSOR,
    PROVIDER_IDS.ANTIGRAVITY,
    PROVIDER_IDS.CLAUDE,
    PROVIDER_IDS.GROK,
]);

/**
 * @typedef {object} UsageMeter
 * @property {string} id
 * @property {string} title
 * @property {number|null} used
 * @property {number|null} left
 * @property {number|null} limit
 * @property {number|null} percent used fraction 0..1
 * @property {number|null} leftPercent remaining fraction 0..1
 * @property {'percent'|'usd'|'credits'|'requests'|'tokens'} unit
 * @property {number|null} resetAt unix seconds
 * @property {number|null} resetAfterSeconds
 * @property {number|null} windowSeconds
 */

/**
 * @typedef {object} ProviderSnapshot
 * @property {string} id
 * @property {string} label
 * @property {'ok'|'auth'|'error'|'disabled'} status
 * @property {string|null} error
 * @property {string|null} account
 * @property {string|null} plan
 * @property {UsageMeter[]} meters
 * @property {object|null} raw
 * @property {number|null} [staleAgeSecs] CLI cache age when serving stale data
 */

/**
 * Map snake_case CLI JSON (`usg json`) into the camelCase UI snapshot shape.
 * @param {object} raw
 * @returns {ProviderSnapshot}
 */
export function normalizeCliSnapshot(raw) {
    const id = String(raw?.id ?? 'unknown');
    const label = String(raw?.label ?? PROVIDER_LABELS[id] ?? id);
    const meters = Array.isArray(raw?.meters)
        ? raw.meters.map(m => createMeter({
            id: String(m?.id ?? 'meter'),
            title: String(m?.title ?? m?.id ?? 'Meter'),
            used: m?.used ?? null,
            left: m?.left ?? null,
            limit: m?.limit ?? null,
            percent: m?.percent ?? null,
            leftPercent: m?.left_percent ?? m?.leftPercent ?? null,
            unit: m?.unit ?? 'percent',
            resetAt: m?.reset_at ?? m?.resetAt ?? null,
            resetAfterSeconds: m?.reset_after_seconds ?? m?.resetAfterSeconds ?? null,
            windowSeconds: m?.window_seconds ?? m?.windowSeconds ?? null,
        }))
        : [];

    const snap = createSnapshot({
        id,
        label,
        status: normalizeStatus(raw?.status),
        error: raw?.error ?? null,
        account: raw?.account ?? null,
        plan: raw?.plan ?? null,
        meters,
        raw: null,
    });
    const stale = coerceNumber(raw?.stale_age_secs ?? raw?.staleAgeSecs);
    if (stale !== null)
        snap.staleAgeSecs = stale;
    return snap;
}

function normalizeStatus(value) {
    const s = String(value ?? 'ok').toLowerCase();
    if (s === 'ok' || s === 'auth' || s === 'error' || s === 'disabled')
        return s;
    return 'error';
}

/**
 * @param {Partial<ProviderSnapshot> & {id: string, label: string}} partial
 * @returns {ProviderSnapshot}
 */
export function createSnapshot(partial) {
    return {
        id: partial.id,
        label: partial.label,
        status: partial.status ?? 'ok',
        error: partial.error ?? null,
        account: partial.account ?? null,
        plan: partial.plan ?? null,
        meters: Array.isArray(partial.meters) ? partial.meters : [],
        raw: partial.raw ?? null,
    };
}

/**
 * @param {Partial<UsageMeter> & {id: string, title: string}} partial
 * @returns {UsageMeter}
 */
export function createMeter(partial) {
    let percent = coerceUnitInterval(partial.percent);
    let leftPercent = coerceUnitInterval(partial.leftPercent);

    if (percent === null && leftPercent !== null)
        percent = clamp01(1 - leftPercent);
    if (leftPercent === null && percent !== null)
        leftPercent = clamp01(1 - percent);

    let used = coerceNumber(partial.used);
    let left = coerceNumber(partial.left);
    let limit = coerceNumber(partial.limit);

    if (used === null && left !== null && limit !== null)
        used = Math.max(limit - left, 0);
    if (left === null && used !== null && limit !== null)
        left = Math.max(limit - used, 0);
    if (percent === null && used !== null && limit !== null && limit > 0)
        percent = clamp01(used / limit);
    if (leftPercent === null && percent !== null)
        leftPercent = clamp01(1 - percent);

    return {
        id: partial.id,
        title: partial.title,
        used,
        left,
        limit,
        percent,
        leftPercent,
        unit: partial.unit ?? 'percent',
        resetAt: coerceNumber(partial.resetAt),
        resetAfterSeconds: coerceNumber(partial.resetAfterSeconds),
        windowSeconds: coerceNumber(partial.windowSeconds),
    };
}

/**
 * Build a percent-based meter from remaining fraction (1 = full / unused).
 * @param {{id: string, title: string, remainingFraction: number, resetAt?: number|null, windowSeconds?: number|null}} args
 */
export function meterFromRemainingFraction({
    id,
    title,
    remainingFraction,
    resetAt = null,
    windowSeconds = null,
}) {
    const leftPercent = clamp01(remainingFraction);
    return createMeter({
        id,
        title,
        percent: 1 - leftPercent,
        leftPercent,
        used: (1 - leftPercent) * 100,
        left: leftPercent * 100,
        limit: 100,
        unit: 'percent',
        resetAt,
        windowSeconds,
    });
}

/**
 * Build a percent-based meter from used percentage (0-100 or 0-1).
 */
export function meterFromUsedPercent({
    id,
    title,
    usedPercent,
    resetAt = null,
    windowSeconds = null,
}) {
    let fraction = coerceNumber(usedPercent);
    if (fraction === null)
        return createMeter({id, title, unit: 'percent', resetAt, windowSeconds});

    if (fraction > 1)
        fraction = fraction / 100;

    fraction = clamp01(fraction);
    return createMeter({
        id,
        title,
        percent: fraction,
        leftPercent: 1 - fraction,
        used: fraction * 100,
        left: (1 - fraction) * 100,
        limit: 100,
        unit: 'percent',
        resetAt,
        windowSeconds,
    });
}

/**
 * @typedef {object} TokenCounts
 * @property {number} inputTokens
 * @property {number} outputTokens
 * @property {number} cacheReadTokens
 * @property {number} cacheWriteTokens
 */

/**
 * @typedef {object} TokenLedger
 * @property {string} period e.g. 'today', 'week', 'month'
 * @property {TokenCounts} totals
 * @property {(TokenCounts & {provider: string})[]} byProvider
 */

const TOKEN_KEYS = {
    inputTokens: ['input_tokens', 'inputTokens', 'input'],
    outputTokens: ['output_tokens', 'outputTokens', 'output'],
    cacheReadTokens: ['cache_read_tokens', 'cacheReadTokens', 'cache_read', 'cacheRead'],
    cacheWriteTokens: ['cache_write_tokens', 'cacheWriteTokens', 'cache_write', 'cacheWrite'],
};

function firstNumber(raw, keys) {
    for (const key of keys) {
        const number = coerceNumber(raw?.[key]);
        if (number !== null)
            return number;
    }
    return null;
}

function normalizeTokenCounts(raw) {
    if (!raw || typeof raw !== 'object')
        return null;
    const counts = {};
    let seen = false;
    for (const [field, keys] of Object.entries(TOKEN_KEYS)) {
        const number = firstNumber(raw, keys);
        if (number !== null)
            seen = true;
        counts[field] = number ?? 0;
    }
    return seen ? counts : null;
}

function isEmptyTokenCounts(counts) {
    return !counts || (
        counts.inputTokens === 0 &&
        counts.outputTokens === 0 &&
        counts.cacheReadTokens === 0 &&
        counts.cacheWriteTokens === 0
    );
}

/**
 * Normalize `usg tokens --json` output into a TokenLedger.
 * Tolerates `totals`/`total`, `by_provider`/`providers`/`byProvider`, and
 * numeric-string counts. Returns null when nothing usable is present so the
 * UI can hide the row instead of showing zeros.
 * @param {object|object[]} raw
 * @returns {TokenLedger|null}
 */
export function normalizeTokenLedger(raw) {
    if (!raw || typeof raw !== 'object')
        return null;

    const providersRaw = Array.isArray(raw)
        ? raw
        : (raw.by_provider ?? raw.byProvider ?? raw.providers);

    const byProvider = [];
    if (Array.isArray(providersRaw)) {
        for (const item of providersRaw) {
            const provider = String(item?.provider ?? item?.id ?? item?.name ?? '').trim();
            const counts = normalizeTokenCounts(item);
            if (!provider || !counts || isEmptyTokenCounts(counts))
                continue;
            byProvider.push({provider, ...counts});
        }
    }
    byProvider.sort((a, b) =>
        (b.inputTokens + b.outputTokens) - (a.inputTokens + a.outputTokens));

    let totals = normalizeTokenCounts(Array.isArray(raw) ? null : (raw.totals ?? raw.total));
    if ((!totals || isEmptyTokenCounts(totals)) && byProvider.length > 0) {
        totals = {inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0};
        for (const entry of byProvider) {
            totals.inputTokens += entry.inputTokens;
            totals.outputTokens += entry.outputTokens;
            totals.cacheReadTokens += entry.cacheReadTokens;
            totals.cacheWriteTokens += entry.cacheWriteTokens;
        }
    }
    if (!totals || (isEmptyTokenCounts(totals) && byProvider.length === 0))
        return null;

    const period = typeof raw?.period === 'string' && raw.period.trim()
        ? raw.period.trim()
        : 'today';

    return {period, totals, byProvider};
}

/**
 * Human-readable token count with k/M/B suffixes: 340000 → '340k',
 * 1200000 → '1.2M'.
 * @param {number|string|null} value
 * @returns {string}
 */
export function formatTokenCount(value) {
    const number = coerceNumber(value) ?? 0;
    const sign = number < 0 ? '-' : '';
    const abs = Math.abs(number);
    for (const [divisor, suffix] of [[1e9, 'B'], [1e6, 'M'], [1e3, 'k']]) {
        if (abs >= divisor) {
            const text = (abs / divisor).toFixed(1).replace(/\.0$/, '');
            return `${sign}${text}${suffix}`;
        }
    }
    return `${sign}${Math.round(abs)}`;
}

export function coerceNumber(value) {
    if (typeof value === 'number' && Number.isFinite(value))
        return value;
    if (typeof value === 'string' && value.trim()) {
        const parsed = Number.parseFloat(value);
        if (Number.isFinite(parsed))
            return parsed;
    }
    return null;
}

export function coerceUnixSeconds(value) {
    if (typeof value === 'number' && Number.isFinite(value))
        return value > 9999999999 ? value / 1000 : value;

    if (typeof value !== 'string' || !value.trim())
        return null;

    const trimmed = value.trim();
    if (/^-?\d+(\.\d+)?$/.test(trimmed)) {
        const number = Number.parseFloat(trimmed);
        return number > 9999999999 ? number / 1000 : number;
    }

    const timestamp = Date.parse(trimmed);
    return Number.isFinite(timestamp) ? timestamp / 1000 : null;
}

function coerceUnitInterval(value) {
    const number = coerceNumber(value);
    if (number === null)
        return null;
    if (number > 1 && number <= 100)
        return clamp01(number / 100);
    return clamp01(number);
}

function clamp01(value) {
    return Math.max(0, Math.min(1, value));
}
