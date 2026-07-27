export const PROVIDER_IDS = Object.freeze({
    CODEX: 'codex',
    CURSOR: 'cursor',
    ANTIGRAVITY: 'antigravity',
});

export const PROVIDER_LABELS = Object.freeze({
    [PROVIDER_IDS.CODEX]: 'Codex',
    [PROVIDER_IDS.CURSOR]: 'Cursor',
    [PROVIDER_IDS.ANTIGRAVITY]: 'Antigravity',
});

export const DEFAULT_ENABLED_PROVIDERS = Object.freeze([
    PROVIDER_IDS.CODEX,
    PROVIDER_IDS.CURSOR,
    PROVIDER_IDS.ANTIGRAVITY,
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
 */

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
