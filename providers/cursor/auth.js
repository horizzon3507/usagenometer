import GLib from 'gi://GLib';

import {runCommand} from '../../lib/asyncSubprocess.js';

const EXPIRY_SKEW_SECONDS = 30;

export class CursorAuthError extends Error {
    constructor(message, {path = null, expired = false} = {}) {
        super(message);
        this.name = 'CursorAuthError';
        this.path = path;
        this.expired = expired;
    }
}

export function getCursorStateDbPath() {
    return GLib.build_filenamev([
        GLib.get_user_config_dir(),
        'Cursor',
        'User',
        'globalStorage',
        'state.vscdb',
    ]);
}

/**
 * Read Cursor session JWT from local VS Code state DB.
 * @returns {Promise<{accessToken: string, userId: string, email: string|null, membershipType: string|null, expiresAt: number|null, sessionCookie: string, path: string}>}
 */
export async function loadCursorAuth({allowExpired = false} = {}) {
    const path = getCursorStateDbPath();
    const values = await readStateKeys(path, [
        'cursorAuth/accessToken',
        'cursorAuth/cachedEmail',
        'cursorAuth/stripeMembershipType',
        'cursorAuth/stripeSubscriptionStatus',
    ]);

    const accessToken = normalizeToken(values['cursorAuth/accessToken']);
    if (!accessToken) {
        throw new CursorAuthError(
            `Cursor auth not found at ${path}. Sign in to the Cursor app.`,
            {path},
        );
    }

    const userId = getUserIdFromToken(accessToken);
    if (!userId) {
        throw new CursorAuthError(
            'Cursor access token is missing a user id subject.',
            {path},
        );
    }

    const expiresAt = getJwtExpiresAt(accessToken);
    const now = Math.floor(Date.now() / 1000);
    if (!allowExpired && expiresAt !== null && expiresAt <= now + EXPIRY_SKEW_SECONDS) {
        throw new CursorAuthError(
            'Cursor session token is expired. Open Cursor and sign in again.',
            {path, expired: true},
        );
    }

    return {
        accessToken,
        userId,
        email: normalizeString(values['cursorAuth/cachedEmail']),
        membershipType: normalizeString(values['cursorAuth/stripeMembershipType']),
        subscriptionStatus: normalizeString(values['cursorAuth/stripeSubscriptionStatus']),
        expiresAt,
        expiresInSeconds: expiresAt !== null ? Math.max(expiresAt - now, 0) : null,
        sessionCookie: `WorkosCursorSessionToken=${userId}%3A%3A${accessToken}`,
        path,
    };
}

async function readStateKeys(path, keys) {
    const file = GioFileExists(path);
    if (!file) {
        throw new CursorAuthError(
            `Cursor state database not found at ${path}. Sign in to the Cursor app.`,
            {path},
        );
    }

    // Prefer python sqlite3 (always available with GNOME stacks) over sqlite3 CLI.
    const script = `
import sqlite3, json, sys
path = sys.argv[1]
keys = json.loads(sys.argv[2])
con = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
cur = con.cursor()
out = {}
for key in keys:
    row = cur.execute("SELECT value FROM ItemTable WHERE key=?", (key,)).fetchone()
    out[key] = row[0] if row else None
con.close()
print(json.dumps(out))
`.trim();

    const result = await runCommand([
        'python3', '-c', script, path, JSON.stringify(keys),
    ], {timeoutMs: 10000});

    if (result.status !== 0) {
        throw new CursorAuthError(
            `Failed to read Cursor state database at ${path}.`,
            {path},
        );
    }

    try {
        return JSON.parse(result.stdout.trim() || '{}');
    } catch (error) {
        throw new CursorAuthError(
            `Cursor state database at ${path} returned invalid data.`,
            {path},
        );
    }
}

function GioFileExists(path) {
    return GLib.file_test(path, GLib.FileTest.EXISTS);
}

function getUserIdFromToken(token) {
    const claims = getJwtClaims(token);
    const subject = typeof claims?.sub === 'string' ? claims.sub : '';
    if (!subject)
        return null;
    const parts = subject.split('|');
    const userId = (parts.length > 1 ? parts[1] : parts[0]).trim();
    return userId || null;
}

function getJwtExpiresAt(token) {
    const claims = getJwtClaims(token);
    return typeof claims?.exp === 'number' && Number.isFinite(claims.exp)
        ? claims.exp
        : null;
}

function getJwtClaims(token) {
    const parts = token.split('.');
    if (parts.length !== 3)
        return null;
    try {
        return JSON.parse(decodeBase64Url(parts[1]));
    } catch (error) {
        return null;
    }
}

function decodeBase64Url(value) {
    let normalized = value.replaceAll('-', '+').replaceAll('_', '/');
    while (normalized.length % 4 !== 0)
        normalized += '=';
    return new TextDecoder().decode(GLib.base64_decode(normalized));
}

function normalizeToken(value) {
    if (typeof value !== 'string')
        return '';
    return value.trim().replace(/^Bearer\s+/i, '').trim();
}

function normalizeString(value) {
    return typeof value === 'string' && value.trim() ? value.trim() : null;
}
