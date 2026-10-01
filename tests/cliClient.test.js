#!/usr/bin/env gjs -m
/**
 * Unit tests for CLI JSON → UI snapshot normalizer (no network).
 * Run: gjs -m tests/cliClient.test.js
 */

import {
    normalizeCliSnapshot,
    normalizeTokenLedger,
    formatTokenCount,
    PROVIDER_IDS,
} from '../providers/types.js';
import {fetchTokens} from '../providers/cliBackend.js';

let failed = 0;

function assert(cond, msg) {
    if (!cond) {
        console.error(`FAIL: ${msg}`);
        failed += 1;
    } else {
        console.log(`ok: ${msg}`);
    }
}

const raw = {
    id: 'cursor',
    label: 'Cursor',
    status: 'ok',
    error: null,
    account: 'a@b.c',
    plan: 'pro',
    stale_age_secs: 120,
    meters: [{
        id: 'auto',
        title: 'Auto + Composer',
        used: 73.0,
        left: 27.0,
        limit: 100.0,
        percent: 0.73,
        left_percent: 0.27,
        unit: 'percent',
        reset_at: 1722817938.0,
        reset_after_seconds: null,
        window_seconds: null,
    }],
};

const snap = normalizeCliSnapshot(raw);
assert(snap.id === PROVIDER_IDS.CURSOR, 'id');
assert(snap.status === 'ok', 'status');
assert(snap.account === 'a@b.c', 'account');
assert(snap.staleAgeSecs === 120, 'staleAgeSecs');
assert(snap.meters.length === 1, 'meters length');
assert(snap.meters[0].leftPercent === 0.27, 'leftPercent camelCase');
assert(snap.meters[0].resetAt === 1722817938.0, 'resetAt camelCase');

const missing = normalizeCliSnapshot({id: 'newtool', label: 'New Tool', status: 'auth', meters: []});
assert(missing.id === 'newtool', 'unknown provider id passes through');
assert(missing.status === 'auth', 'auth status');

// --- usg tokens --json → TokenLedger ---

const ledgerRaw = {
    period: 'today',
    totals: {
        input_tokens: 1200000,
        output_tokens: 340000,
        cache_read_tokens: 50000,
        cache_write_tokens: 5000,
    },
    by_provider: [
        {provider: 'claude', input_tokens: 800000, output_tokens: 200000, cache_read_tokens: 50000, cache_write_tokens: 5000},
        {provider: 'codex', input_tokens: 400000, output_tokens: 140000, cache_read_tokens: 0, cache_write_tokens: 0},
    ],
};

const ledger = normalizeTokenLedger(ledgerRaw);
assert(ledger !== null, 'ledger parsed');
assert(ledger.period === 'today', 'ledger period');
assert(ledger.totals.inputTokens === 1200000, 'totals input camelCase');
assert(ledger.totals.outputTokens === 340000, 'totals output camelCase');
assert(ledger.totals.cacheReadTokens === 50000, 'totals cache_read camelCase');
assert(ledger.byProvider.length === 2, 'byProvider length');
assert(ledger.byProvider[0].provider === 'claude', 'byProvider sorted desc');
assert(ledger.byProvider[1].provider === 'codex', 'byProvider second');

// Defensive variants: total/providers aliases, numeric strings, camelCase keys
const variantLedger = normalizeTokenLedger({
    period: 'week',
    total: {input: '2500', output: '1200'},
    providers: [
        {id: 'grok', inputTokens: '2500', outputTokens: '1200'},
        {name: '', input_tokens: 9}, // dropped: no provider name
        {provider: 'claude', input_tokens: 0, output_tokens: 0}, // dropped: all zero
    ],
});
assert(variantLedger !== null, 'variant ledger parsed');
assert(variantLedger.period === 'week', 'variant period');
assert(variantLedger.totals.inputTokens === 2500, 'numeric-string totals coerce');
assert(variantLedger.totals.outputTokens === 1200, 'numeric-string output coerce');
assert(variantLedger.byProvider.length === 1, 'variant drops empty/anonymous providers');
assert(variantLedger.byProvider[0].provider === 'grok', 'variant provider id key');

// Missing totals → summed from providers
const summed = normalizeTokenLedger({
    by_provider: [{provider: 'claude', input_tokens: 10, output_tokens: 5}],
});
assert(summed !== null && summed.totals.inputTokens === 10, 'totals summed from providers');
assert(summed.totals.outputTokens === 5, 'output summed from providers');

// Empty / error payloads degrade to null (row hidden)
assert(normalizeTokenLedger(null) === null, 'null ledger → null');
assert(normalizeTokenLedger({}) === null, 'empty object → null');
assert(normalizeTokenLedger({error: 'unknown command'}) === null, 'error payload → null');
assert(normalizeTokenLedger({totals: {}, by_provider: []}) === null, 'empty totals → null');
assert(normalizeTokenLedger({
    totals: {input_tokens: 0, output_tokens: 0},
}) === null, 'all-zero totals → null');

// --- formatTokenCount (k/M/B) ---
assert(formatTokenCount(0) === '0', 'zero → 0');
assert(formatTokenCount(999) === '999', 'under 1k plain');
assert(formatTokenCount(1000) === '1k', '1k');
assert(formatTokenCount(1500) === '1.5k', '1.5k trims decimal');
assert(formatTokenCount(340000) === '340k', '340k');
assert(formatTokenCount(1200000) === '1.2M', '1.2M');
assert(formatTokenCount(2500000) === '2.5M', '2.5M');
assert(formatTokenCount(3200000000) === '3.2B', '3.2B');
assert(formatTokenCount('42000') === '42k', 'numeric string');
assert(formatTokenCount(null) === '0', 'null → 0');

// --- missing-command degradation: fetchTokens never throws ---
assert((await fetchTokens({binaryPath: null})) === null, 'no binary → null');
assert((await fetchTokens({binaryPath: '/nonexistent/usg-binary'})) === null, 'unrunnable binary → null');
assert((await fetchTokens({binaryPath: '/bin/false'})) === null, 'probe exits nonzero → null');

if (failed > 0) {
    console.error(`${failed} assertion(s) failed`);
    // gjs may not honor process.exit; throw
    throw new Error(`${failed} failed`);
}
console.log('all passed');
