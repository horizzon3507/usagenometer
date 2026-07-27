import GLib from 'gi://GLib';

import {HttpClient, HttpError} from '../../lib/http.js';
import {runCommand} from '../../lib/asyncSubprocess.js';
import {coerceUnixSeconds} from '../types.js';

// Keep OAuth client credentials outside the repository. Configure these in the
// environment used to launch GNOME Shell when token refresh is required.
const GOOGLE_CLIENT_ID = GLib.getenv('USAGENOMETER_GOOGLE_CLIENT_ID') ?? '';
const GOOGLE_CLIENT_SECRET = GLib.getenv('USAGENOMETER_GOOGLE_CLIENT_SECRET') ?? '';
const GOOGLE_TOKEN_URL = 'https://oauth2.googleapis.com/token';
const EXPIRY_SKEW_SECONDS = 60;

export class AntigravityAuthError extends Error {
    constructor(message, {expired = false} = {}) {
        super(message);
        this.name = 'AntigravityAuthError';
        this.expired = expired;
    }
}

/**
 * Load Antigravity OAuth tokens from the desktop secret store (service=gemini, username=antigravity)
 * and refresh when near expiry.
 */
export async function loadAntigravityAuth({allowExpired = false, httpClient = null} = {}) {
    const stored = await readSecretPayload();
    const tokenBlock = stored?.token && typeof stored.token === 'object'
        ? stored.token
        : stored;

    let accessToken = normalizeToken(tokenBlock?.access_token);
    let refreshToken = normalizeToken(tokenBlock?.refresh_token);
    let expiresAt = coerceUnixSeconds(tokenBlock?.expiry ?? tokenBlock?.expiry_date);

    if (!accessToken && !refreshToken) {
        throw new AntigravityAuthError(
            'Antigravity credentials not found. Sign in with the Antigravity app or CLI.',
        );
    }

    const now = Math.floor(Date.now() / 1000);
    const needsRefresh = !accessToken ||
        (expiresAt !== null && expiresAt <= now + EXPIRY_SKEW_SECONDS);

    if (needsRefresh) {
        if (!GOOGLE_CLIENT_ID || !GOOGLE_CLIENT_SECRET) {
            throw new AntigravityAuthError(
                'Antigravity token needs refresh, but OAuth client credentials are not configured.',
                {expired: true},
            );
        }
        if (!refreshToken) {
            throw new AntigravityAuthError(
                'Antigravity access token is expired and no refresh token is available.',
                {expired: true},
            );
        }

        const client = httpClient ?? new HttpClient();
        const ownsClient = !httpClient;
        try {
            const refreshed = await client.postForm(GOOGLE_TOKEN_URL, {
                client_id: GOOGLE_CLIENT_ID,
                client_secret: GOOGLE_CLIENT_SECRET,
                refresh_token: refreshToken,
                grant_type: 'refresh_token',
            });
            accessToken = normalizeToken(refreshed?.access_token);
            if (!accessToken) {
                throw new AntigravityAuthError(
                    'Antigravity token refresh returned no access token.',
                    {expired: true},
                );
            }
            const expiresIn = Number(refreshed?.expires_in);
            expiresAt = Number.isFinite(expiresIn)
                ? now + Math.floor(expiresIn)
                : null;
        } catch (error) {
            if (error instanceof HttpError && error.isAuthError) {
                throw new AntigravityAuthError(
                    'Antigravity refresh token was rejected. Sign in again with Antigravity.',
                    {expired: true},
                );
            }
            if (!allowExpired || !accessToken)
                throw error instanceof Error ? error : new AntigravityAuthError(String(error));
        } finally {
            if (ownsClient)
                client.destroy();
        }
    }

    if (!accessToken) {
        throw new AntigravityAuthError(
            'Antigravity access token is unavailable. Sign in with the Antigravity app or CLI.',
            {expired: true},
        );
    }

    if (!allowExpired && expiresAt !== null && expiresAt <= now + EXPIRY_SKEW_SECONDS) {
        throw new AntigravityAuthError(
            'Antigravity access token is expired. Sign in again with Antigravity.',
            {expired: true},
        );
    }

    const account = await readActiveGoogleAccount();

    return {
        accessToken,
        refreshToken,
        expiresAt,
        expiresInSeconds: expiresAt !== null ? Math.max(expiresAt - now, 0) : null,
        account,
        authMethod: typeof stored?.auth_method === 'string' ? stored.auth_method : null,
    };
}

async function readSecretPayload() {
    // libsecret via secret-tool (standard on GNOME)
    try {
        const result = await runCommand([
            'secret-tool', 'lookup', 'service', 'gemini', 'username', 'antigravity',
        ], {timeoutMs: 8000});

        if (result.status === 0 && result.stdout.trim())
            return parseSecretText(result.stdout.trim());
    } catch (error) {
        // fall through to file fallbacks
    }

    // Fallback: some Gemini CLI installs keep oauth under ~/.gemini
    const oauthPath = GLib.build_filenamev([GLib.get_home_dir(), '.gemini', 'oauth_creds.json']);
    if (GLib.file_test(oauthPath, GLib.FileTest.EXISTS)) {
        try {
            const result = await runCommand(['python3', '-c',
                'import json,sys; print(json.dumps(json.load(open(sys.argv[1]))))', oauthPath],
            {timeoutMs: 5000});
            if (result.status === 0 && result.stdout.trim()) {
                const payload = JSON.parse(result.stdout.trim());
                return {token: payload, auth_method: 'gemini-oauth-file'};
            }
        } catch (error) {
            // ignore
        }
    }

    throw new AntigravityAuthError(
        'Antigravity credentials not found in the secret store (service=gemini, username=antigravity).',
    );
}

function parseSecretText(text) {
    let raw = text.trim();
    if (raw.startsWith('go-keyring-base64:')) {
        const b64 = raw.slice('go-keyring-base64:'.length);
        raw = new TextDecoder().decode(GLib.base64_decode(b64));
    }

    try {
        return JSON.parse(raw);
    } catch (error) {
        if (raw.startsWith('Bearer '))
            return {token: {access_token: raw.slice(7).trim()}};
        if (raw.length > 20)
            return {token: {access_token: raw}};
        throw new AntigravityAuthError('Antigravity secret store payload is not valid JSON.');
    }
}

async function readActiveGoogleAccount() {
    const path = GLib.build_filenamev([GLib.get_home_dir(), '.gemini', 'google_accounts.json']);
    if (!GLib.file_test(path, GLib.FileTest.EXISTS))
        return null;
    try {
        const result = await runCommand(['python3', '-c',
            'import json,sys; print(json.load(open(sys.argv[1])).get("active") or "")', path],
        {timeoutMs: 3000});
        const value = result.stdout.trim();
        return value || null;
    } catch (error) {
        return null;
    }
}

function normalizeToken(value) {
    if (typeof value !== 'string')
        return '';
    return value.trim().replace(/^Bearer\s+/i, '').trim();
}
