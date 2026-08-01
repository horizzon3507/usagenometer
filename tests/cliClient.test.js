#!/usr/bin/env gjs -m
/**
 * Unit tests for CLI JSON → UI snapshot normalizer (no network).
 * Run: gjs -m tests/cliClient.test.js
 */

import {
    normalizeCliSnapshot,
    PROVIDER_IDS,
} from '../providers/types.js';

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

if (failed > 0) {
    console.error(`${failed} assertion(s) failed`);
    // gjs may not honor process.exit; throw
    throw new Error(`${failed} failed`);
}
console.log('all passed');
