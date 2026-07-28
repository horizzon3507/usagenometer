import GLib from 'gi://GLib';

import {HttpClient, HttpError} from '../../lib/http.js';
import {runCommand} from '../../lib/asyncSubprocess.js';
import {fetchAntigravitySnapshot} from '../antigravity/index.js';
import {
    PROVIDER_IDS,
    PROVIDER_LABELS,
    coerceUnixSeconds,
    createMeter,
    createSnapshot,
    meterFromUsedPercent,
} from '../types.js';

const CLAUDE_USAGE_URL = 'https://api.anthropic.com/api/oauth/usage';
const CLAUDE_BETA = 'oauth-2025-04-20';
const GROK_USER_URL = 'https://cli-chat-proxy.grok.com/v1/user';
const GROK_BILLING_CREDITS_URL = 'https://cli-chat-proxy.grok.com/v1/billing?format=credits';
const GROK_BILLING_URL = 'https://cli-chat-proxy.grok.com/v1/billing';

export async function fetchClaudeSnapshot({httpClient = null} = {}) {
    const client = httpClient ?? new HttpClient();
    const ownsClient = !httpClient;
    try {
        try {
            return await fetchClaudeOauthSnapshot(client);
        } catch (oauthError) {
            const ag = await fetchAntigravitySnapshot({httpClient: client});
            if (ag.status === 'ok') {
                const meters = (ag.meters ?? []).filter(m => String(m.id).startsWith('3p-'));
                if (meters.length > 0) {
                    return createSnapshot({
                        id: PROVIDER_IDS.CLAUDE,
                        label: PROVIDER_LABELS[PROVIDER_IDS.CLAUDE],
                        status: 'ok',
                        error: 'via Antigravity Claude/GPT pools',
                        account: ag.account ?? null,
                        plan: 'antigravity',
                        meters,
                    });
                }
            }

            const installed = Boolean(GLib.find_program_in_path('claude'));
            return createSnapshot({
                id: PROVIDER_IDS.CLAUDE,
                label: PROVIDER_LABELS[PROVIDER_IDS.CLAUDE],
                status: installed ? 'auth' : 'error',
                error: installed
                    ? `${formatErr(oauthError)} (run claude login for subscription meters, or use Antigravity)`
                    : 'claude CLI not found in PATH.',
                meters: [],
            });
        }
    } finally {
        if (ownsClient)
            client.destroy();
    }
}

export async function fetchGrokSnapshot({httpClient = null} = {}) {
    const client = httpClient ?? new HttpClient();
    const ownsClient = !httpClient;
    try {
        const auth = await loadGrokAuth();
        const user = await client.getJson(GROK_USER_URL, {
            headers: grokHeaders(auth.accessToken),
        });
        const userId = user?.userId;
        if (!userId)
            throw new Error('Grok /v1/user response missing userId.');

        const credits = await client.getJson(GROK_BILLING_CREDITS_URL, {
            headers: {...grokHeaders(auth.accessToken), 'x-userid': String(userId)},
        });
        let monthly = null;
        try {
            monthly = await client.getJson(GROK_BILLING_URL, {
                headers: {...grokHeaders(auth.accessToken), 'x-userid': String(userId)},
            });
        } catch (error) {
            monthly = null;
        }

        return snapshotFromGrokBilling(credits, monthly, user?.email ?? auth.email);
    } catch (error) {
        const installed = Boolean(GLib.find_program_in_path('grok'));
        const isAuth = error instanceof GrokAuthError ||
            (error instanceof HttpError && error.isAuthError);
        return createSnapshot({
            id: PROVIDER_IDS.GROK,
            label: PROVIDER_LABELS[PROVIDER_IDS.GROK],
            status: isAuth ? 'auth' : 'error',
            error: installed
                ? formatErr(error)
                : (isAuth ? formatErr(error) : 'grok CLI not found in PATH.'),
            meters: [],
        });
    } finally {
        if (ownsClient)
            client.destroy();
    }
}

export async function testClaudeConnection() {
    const snapshot = await fetchClaudeSnapshot();
    return {
        ok: snapshot.status === 'ok',
        message: snapshot.status === 'ok'
            ? (snapshot.account
                ? `Connected as ${snapshot.account}`
                : (snapshot.meters.length
                    ? `Connected · ${snapshot.meters.length} meter(s)`
                    : (snapshot.error ?? 'Connection OK')))
            : (snapshot.error ?? 'Claude connection failed'),
        snapshot,
    };
}

export async function testGrokConnection() {
    const snapshot = await fetchGrokSnapshot();
    return {
        ok: snapshot.status === 'ok',
        message: snapshot.status === 'ok'
            ? (snapshot.account
                ? `Connected as ${snapshot.account}`
                : (snapshot.meters.length
                    ? `Connected · ${snapshot.meters.length} meter(s)`
                    : (snapshot.error ?? 'Connection OK')))
            : (snapshot.error ?? 'Grok connection failed'),
        snapshot,
    };
}

async function fetchClaudeOauthSnapshot(client) {
    const auth = await loadClaudeOauthAuth();
    const summary = await client.getJson(CLAUDE_USAGE_URL, {
        headers: {
            Accept: 'application/json',
            Authorization: `Bearer ${auth.accessToken}`,
            'anthropic-beta': CLAUDE_BETA,
            'User-Agent': 'usagenometer/1.0',
        },
    });
    return snapshotFromClaudeOauth(summary, auth);
}

function snapshotFromClaudeOauth(summary, auth) {
    const meters = [];
    pushClaudeUtil(meters, summary?.five_hour, 'five_hour', '5 hour');
    pushClaudeUtil(meters, summary?.seven_day, 'seven_day', 'Weekly');
    pushClaudeUtil(meters, summary?.seven_day_sonnet, 'seven_day_sonnet', 'Weekly · Sonnet');
    pushClaudeUtil(meters, summary?.seven_day_opus, 'seven_day_opus', 'Weekly · Opus');

    for (const [index, limit] of (summary?.limits ?? []).entries()) {
        const kind = limit?.kind ?? 'limit';
        const model = limit?.scope?.model?.display_name;
        const title = kind === 'session'
            ? '5 hour'
            : kind === 'weekly_all'
                ? 'Weekly'
                : kind === 'weekly_scoped' && model
                    ? `Weekly · ${model}`
                    : model
                        ? `${kind} · ${model}`
                        : String(kind);
        const percent = limit?.percent ?? limit?.utilization;
        if (typeof percent !== 'number' || !Number.isFinite(percent))
            continue;
        meters.push(meterFromUsedPercent({
            id: `${kind}-${model ?? index}`,
            title,
            usedPercent: percent,
            resetAt: coerceUnixSeconds(limit?.resets_at ?? limit?.resetsAt),
        }));
    }

    return createSnapshot({
        id: PROVIDER_IDS.CLAUDE,
        label: PROVIDER_LABELS[PROVIDER_IDS.CLAUDE],
        status: 'ok',
        error: meters.length ? null : 'Claude OAuth connected, but no quota buckets were returned.',
        plan: auth.subscriptionType ?? null,
        meters,
    });
}

function pushClaudeUtil(meters, value, id, title) {
    if (!value || typeof value.utilization !== 'number')
        return;
    meters.push(meterFromUsedPercent({
        id,
        title,
        usedPercent: value.utilization,
        resetAt: coerceUnixSeconds(value.resets_at),
    }));
}

function snapshotFromGrokBilling(credits, monthly, email) {
    const config = credits?.config ?? credits ?? {};
    const periodEnd = coerceUnixSeconds(config?.currentPeriod?.end ?? config?.billingPeriodEnd);
    const meters = [];

    if (typeof config.creditUsagePercent === 'number') {
        meters.push(meterFromUsedPercent({
            id: 'weekly_credits',
            title: 'Weekly credits',
            usedPercent: config.creditUsagePercent,
            resetAt: periodEnd,
        }));
    }

    for (const product of config.productUsage ?? []) {
        if (typeof product?.usagePercent !== 'number')
            continue;
        const name = product.product ?? 'Product';
        meters.push(meterFromUsedPercent({
            id: `product_${String(name).toLowerCase()}`,
            title: humanizeGrokProduct(name),
            usedPercent: product.usagePercent,
            resetAt: periodEnd,
        }));
    }

    const onCap = Number(config?.onDemandCap?.val ?? 0);
    const onUsed = Number(config?.onDemandUsed?.val ?? 0);
    if (onCap > 0) {
        meters.push(createMeter({
            id: 'on_demand',
            title: 'On-demand',
            used: onUsed,
            left: Math.max(onCap - onUsed, 0),
            limit: onCap,
            percent: onUsed / onCap,
            leftPercent: Math.max(1 - onUsed / onCap, 0),
            unit: 'credits',
            resetAt: periodEnd,
        }));
    }

    if (meters.length === 0 && monthly) {
        const mcfg = monthly.config ?? monthly;
        const used = Number(mcfg?.used?.val);
        const limit = Number(mcfg?.monthlyLimit?.val);
        const resetAt = coerceUnixSeconds(mcfg?.billingPeriodEnd);
        if (Number.isFinite(used) && Number.isFinite(limit) && limit > 0) {
            meters.push(createMeter({
                id: 'monthly',
                title: 'Monthly',
                used,
                left: Math.max(limit - used, 0),
                limit,
                percent: used / limit,
                leftPercent: Math.max(1 - used / limit, 0),
                unit: 'credits',
                resetAt,
            }));
        } else if (Number.isFinite(used) && used > 0) {
            meters.push(createMeter({
                id: 'monthly_used',
                title: 'Monthly used',
                used,
                unit: 'credits',
                resetAt,
            }));
        }
    }

    return createSnapshot({
        id: PROVIDER_IDS.GROK,
        label: PROVIDER_LABELS[PROVIDER_IDS.GROK],
        status: 'ok',
        error: meters.length
            ? null
            : 'Logged in, but this Grok plan did not expose percentage quotas.',
        account: email ?? null,
        plan: credits?.subscriptionTier ?? credits?.planName ?? null,
        meters,
    });
}

function humanizeGrokProduct(name) {
    if (name === 'GrokBuild')
        return 'Grok Build';
    if (name === 'GrokChat')
        return 'Grok Chat';
    if (name === 'Api')
        return 'API';
    return String(name);
}

function grokHeaders(token) {
    return {
        Accept: 'application/json',
        Authorization: `Bearer ${token}`,
        'User-Agent': 'usagenometer/1.0',
        'X-XAI-Token-Auth': 'xai-grok-cli',
    };
}

class ClaudeAuthError extends Error {
    constructor(message) {
        super(message);
        this.name = 'ClaudeAuthError';
    }
}

class GrokAuthError extends Error {
    constructor(message) {
        super(message);
        this.name = 'GrokAuthError';
    }
}

async function loadClaudeOauthAuth() {
    const path = GLib.build_filenamev([GLib.get_home_dir(), '.claude', '.credentials.json']);
    let payload = null;
    if (GLib.file_test(path, GLib.FileTest.EXISTS)) {
        const result = await runCommand(['python3', '-c',
            'import json,sys; print(json.dumps(json.load(open(sys.argv[1]))))', path],
        {timeoutMs: 5000});
        if (result.status === 0 && result.stdout.trim())
            payload = JSON.parse(result.stdout.trim());
    }
    if (!payload) {
        try {
            const result = await runCommand([
                'secret-tool', 'lookup', 'service', 'Claude Code-credentials',
            ], {timeoutMs: 8000});
            if (result.status === 0 && result.stdout.trim())
                payload = JSON.parse(result.stdout.trim());
        } catch (error) {
            // ignore
        }
    }
    if (!payload)
        throw new ClaudeAuthError('Claude OAuth credentials not found. Run claude login.');

    const oauth = payload.claudeAiOauth ?? payload;
    const accessToken = String(oauth.accessToken ?? oauth.access_token ?? '').trim();
    if (!accessToken)
        throw new ClaudeAuthError('Claude OAuth access token missing. Run claude login.');

    return {
        accessToken,
        subscriptionType: oauth.subscriptionType ?? oauth.rateLimitTier ?? null,
    };
}

async function loadGrokAuth() {
    const path = GLib.build_filenamev([GLib.get_home_dir(), '.grok', 'auth.json']);
    if (!GLib.file_test(path, GLib.FileTest.EXISTS))
        throw new GrokAuthError(`Grok auth not found at ${path}. Run grok login.`);

    const result = await runCommand(['python3', '-c',
        'import json,sys; print(json.dumps(json.load(open(sys.argv[1]))))', path],
    {timeoutMs: 5000});
    if (result.status !== 0 || !result.stdout.trim())
        throw new GrokAuthError(`Failed to read Grok auth at ${path}.`);

    const payload = JSON.parse(result.stdout.trim());
    const entry = Object.values(payload ?? {}).find(v => v && typeof v === 'object' && v.key);
    const accessToken = String(entry?.key ?? '').trim();
    if (!accessToken)
        throw new GrokAuthError('Grok session token missing. Run grok login.');

    return {
        accessToken,
        email: typeof entry.email === 'string' ? entry.email : null,
    };
}

function formatErr(error) {
    if (error instanceof Error)
        return error.message;
    return String(error);
}
