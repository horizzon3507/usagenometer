import {normalizeSummary} from '../usageApi.js';
import {snapshotFromUsageSummary} from '../providers/cursor/index.js';
import {snapshotFromQuotaSummary} from '../providers/antigravity/index.js';
import {meterFromRemainingFraction, meterFromUsedPercent} from '../providers/types.js';

function assertEqual(actual, expected, message) {
    if (!Object.is(actual, expected))
        throw new Error(`${message}: expected ${expected}, got ${actual}`);
}

function assert(condition, message) {
    if (!condition)
        throw new Error(message);
}

// --- Codex / usageApi ---
const exhaustedWeeklyLimit = normalizeSummary({
    rate_limit: {
        primary_window: {
            used_percent: 100,
            window_seconds: 7 * 86400,
        },
    },
});

assertEqual(exhaustedWeeklyLimit.weekWindow?.percent, 1,
    'weekly window should be fully used');
assertEqual(exhaustedWeeklyLimit.weekWindow?.leftPercent, 0,
    'weekly window should have no usage remaining');
assertEqual(exhaustedWeeklyLimit.percent, 1,
    'summary should preserve the normalized weekly percentage');
assertEqual(exhaustedWeeklyLimit.leftPercent, 0,
    'summary should match the exhausted weekly window');

// --- Cursor dual pool ---
const cursorSnapshot = snapshotFromUsageSummary({
    membershipType: 'pro',
    billingCycleEnd: '2026-08-05T01:12:18.000Z',
    individualUsage: {
        plan: {
            autoPercentUsed: 73.67,
            apiPercentUsed: 36.644444444444446,
            totalPercentUsed: 68.84,
        },
    },
}, {email: 'user@example.com'});

assertEqual(cursorSnapshot.status, 'ok', 'cursor snapshot ok');
assertEqual(cursorSnapshot.account, 'user@example.com', 'cursor account');
assertEqual(cursorSnapshot.meters.length, 2, 'cursor dual meters');
assertEqual(cursorSnapshot.meters[0].id, 'auto_composer', 'auto meter id');
assertEqual(cursorSnapshot.meters[1].id, 'api', 'api meter id');
assert(
    Math.abs(cursorSnapshot.meters[0].percent - 0.7367) < 0.001,
    `auto percent expected ~0.7367 got ${cursorSnapshot.meters[0].percent}`,
);
assert(
    Math.abs(cursorSnapshot.meters[1].percent - 0.3664) < 0.001,
    `api percent expected ~0.3664 got ${cursorSnapshot.meters[1].percent}`,
);

// --- Antigravity quota summary ---
const agSnapshot = snapshotFromQuotaSummary({
    groups: [
        {
            displayName: 'Gemini Models',
            buckets: [
                {
                    bucketId: 'gemini-weekly',
                    displayName: 'Weekly Limit',
                    remainingFraction: 0.98,
                    resetTime: '2026-07-31T14:47:51Z',
                },
                {
                    bucketId: 'gemini-5h',
                    displayName: 'Five Hour Limit',
                    remainingFraction: 1,
                    resetTime: '2026-07-27T04:32:50Z',
                },
            ],
        },
        {
            displayName: 'Claude and GPT models',
            buckets: [
                {
                    bucketId: '3p-5h',
                    displayName: 'Five Hour Limit',
                    remainingFraction: 0.5,
                },
                {
                    bucketId: '3p-weekly',
                    displayName: 'Weekly Limit',
                    remainingFraction: 1,
                },
            ],
        },
    ],
}, {account: 'ag@example.com'});

assertEqual(agSnapshot.status, 'ok', 'ag snapshot ok');
assertEqual(agSnapshot.meters.length, 4, 'ag four buckets');
assertEqual(agSnapshot.meters[0].id, 'gemini-5h', 'gemini 5h first');
assertEqual(agSnapshot.meters[0].percent, 0, 'gemini 5h unused');
assertEqual(agSnapshot.meters[2].id, '3p-5h', '3p 5h');
assertEqual(agSnapshot.meters[2].percent, 0.5, '3p 5h half used');

// --- helpers ---
const leftMeter = meterFromUsedPercent({id: 't', title: 'T', usedPercent: 25});
assertEqual(leftMeter.leftPercent, 0.75, 'left from used percent');
const remMeter = meterFromRemainingFraction({id: 'r', title: 'R', remainingFraction: 0.2});
assertEqual(remMeter.percent, 0.8, 'used from remaining fraction');

print('usageApi + provider tests passed');
