/**
 * Thin CLI bridge — GNOME shells out to `usg` / `usagenometer` for all
 * provider fetch/test. No per-provider HTTP or auth logic lives here.
 */

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

import {runCommand} from '../lib/asyncSubprocess.js';
import {
    PROVIDER_IDS,
    PROVIDER_LABELS,
    createSnapshot,
    normalizeCliSnapshot,
    normalizeTokenLedger,
} from './types.js';

const CLI_CANDIDATES = ['usg', 'usagenometer'];
const JSON_TIMEOUT_MS = 60000;
const TEST_TIMEOUT_MS = 45000;
const LIST_TIMEOUT_MS = 10000;
const TOKENS_TIMEOUT_MS = 30000;
const TOKENS_PROBE_TIMEOUT_MS = 10000;
const TOKENS_CAPABILITY_FILENAME = 'gnome-tokens-capability.json';

/** @type {string|null|undefined} */
let _resolvedBinary;

/** @type {{bin: string, argv: string[]|null}|undefined} */
let _tokensCommand;

/**
 * Resolve `usg` or `usagenometer` on PATH (cached).
 * @returns {string|null}
 */
export function resolveCliBinary() {
    if (_resolvedBinary !== undefined)
        return _resolvedBinary;

    for (const name of CLI_CANDIDATES) {
        const path = GLib.find_program_in_path(name);
        if (path) {
            _resolvedBinary = path;
            return path;
        }
    }
    _resolvedBinary = null;
    return null;
}

/** Clear cached binary path and tokens capability (tests / after install). */
export function resetCliBinaryCache() {
    _resolvedBinary = undefined;
    _tokensCommand = undefined;
}

/**
 * @returns {string}
 */
export function cliMissingMessage() {
    return 'usg / usagenometer not found on PATH. Install the CLI (cargo install usagenometer or AUR), then re-open this session.';
}

/**
 * @param {string[]} extraArgs
 * @param {{timeoutMs?: number}} [options]
 */
async function runCli(extraArgs, {timeoutMs = JSON_TIMEOUT_MS} = {}) {
    const bin = resolveCliBinary();
    if (!bin)
        throw new Error(cliMissingMessage());

    const argv = [bin, ...extraArgs];
    const result = await runCommand(argv, {timeoutMs});
    return {bin, argv, ...result};
}

/**
 * Parse `usg providers -q` stdout into `{id, label}[]`.
 * @param {string} stdout
 * @returns {{id: string, label: string}[]}
 */
export function parseProvidersList(stdout) {
    const defs = [];
    for (const line of String(stdout).split('\n')) {
        const trimmed = line.trim();
        if (!trimmed)
            continue;
        const parts = trimmed.split(/\s+/);
        if (parts.length < 1)
            continue;
        const id = parts[0].toLowerCase();
        if (!/^[a-z][a-z0-9_-]*$/.test(id))
            continue;
        const label = parts.slice(1).join(' ') || (PROVIDER_LABELS[id] ?? titleCase(id));
        defs.push({id, label});
    }
    return defs;
}

/**
 * Discover providers from the CLI. Falls back to the static catalog.
 * @returns {Promise<{id: string, label: string}[]>}
 */
export async function discoverProviderDefs() {
    try {
        const {stdout, status} = await runCli(['providers', '-q'], {timeoutMs: LIST_TIMEOUT_MS});
        if (status === 0) {
            const parsed = parseProvidersList(stdout);
            if (parsed.length > 0)
                return parsed;
        }
    } catch (_error) {
        // fall through
    }
    return Object.values(PROVIDER_IDS).map(id => ({
        id,
        label: PROVIDER_LABELS[id] ?? titleCase(id),
    }));
}

/**
 * Fetch snapshots via `usg json -q [-p id ...]`.
 * @param {string[]} enabledIds
 * @returns {Promise<import('./types.js').ProviderSnapshot[]>}
 */
export async function fetchSnapshotsFromCli(enabledIds) {
    const ids = Array.isArray(enabledIds) ? enabledIds.filter(Boolean) : [];
    if (ids.length === 0) {
        return Object.values(PROVIDER_IDS).map(id => createSnapshot({
            id,
            label: PROVIDER_LABELS[id] ?? titleCase(id),
            status: 'disabled',
            error: 'Disabled',
            meters: [],
        }));
    }

    const bin = resolveCliBinary();
    if (!bin) {
        return ids.map(id => createSnapshot({
            id,
            label: PROVIDER_LABELS[id] ?? titleCase(id),
            status: 'error',
            error: cliMissingMessage(),
            meters: [],
        }));
    }

    const args = ['json', '-q'];
    for (const id of ids)
        args.push('-p', id);

    try {
        const {stdout, stderr, status} = await runCli(args, {timeoutMs: JSON_TIMEOUT_MS});
        if (status !== 0) {
            const detail = (stderr || stdout || `exit ${status}`).trim().split('\n')[0];
            return ids.map(id => createSnapshot({
                id,
                label: PROVIDER_LABELS[id] ?? titleCase(id),
                status: 'error',
                error: detail || `usg json failed (exit ${status})`,
                meters: [],
            }));
        }

        const raw = JSON.parse(stdout.trim() || '[]');
        if (!Array.isArray(raw))
            throw new Error('usg json did not return an array');

        const byId = new Map();
        for (const item of raw) {
            const snap = normalizeCliSnapshot(item);
            byId.set(snap.id, snap);
        }

        // Preserve enabled order; synthesize missing entries.
        return ids.map(id => {
            if (byId.has(id))
                return byId.get(id);
            return createSnapshot({
                id,
                label: PROVIDER_LABELS[id] ?? titleCase(id),
                status: 'error',
                error: 'No snapshot from usg json',
                meters: [],
            });
        });
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return ids.map(id => createSnapshot({
            id,
            label: PROVIDER_LABELS[id] ?? titleCase(id),
            status: 'error',
            error: message,
            meters: [],
        }));
    }
}

function tokensCapabilityPath() {
    return GLib.build_filenamev([
        GLib.get_user_cache_dir(),
        'usagenometer',
        TOKENS_CAPABILITY_FILENAME,
    ]);
}

function binaryMtimeSeconds(bin) {
    try {
        const info = Gio.File.new_for_path(bin).query_info(
            'time::modified',
            Gio.FileQueryInfoFlags.NONE,
            null,
        );
        const modified = info.get_modification_date_time();
        return modified ? modified.to_unix() : null;
    } catch (_error) {
        return null;
    }
}

function readCachedTokensArgv(bin) {
    try {
        const path = tokensCapabilityPath();
        const [ok, contents] = GLib.file_get_contents(path);
        if (!ok)
            return undefined;
        const cached = JSON.parse(new TextDecoder().decode(contents));
        if (cached?.bin !== bin || cached?.mtime !== binaryMtimeSeconds(bin))
            return undefined;
        if (!Array.isArray(cached?.argv) && cached?.argv !== null)
            return undefined;
        return cached.argv;
    } catch (_error) {
        return undefined;
    }
}

function writeCachedTokensArgv(bin, argv) {
    try {
        const path = tokensCapabilityPath();
        GLib.mkdir_with_parents(GLib.path_get_dirname(path), 0o700);
        GLib.file_set_contents(path, JSON.stringify({
            bin,
            mtime: binaryMtimeSeconds(bin),
            argv,
        }));
    } catch (_error) {
        // capability caching is best-effort
    }
}

/**
 * Probe the CLI once for token ledger support and cache the argv to run
 * (null = unsupported) under the extension cache dir, keyed by binary path +
 * mtime so an upgraded `usg` re-probes automatically.
 * @param {string} bin
 * @returns {Promise<string[]|null>}
 */
async function detectTokensArgv(bin) {
    try {
        const {status} = await runCommand(
            [bin, 'tokens', '--help'],
            {timeoutMs: TOKENS_PROBE_TIMEOUT_MS},
        );
        if (status === 0)
            return ['tokens', '--json'];
    } catch (_error) {
        // fall through
    }

    try {
        const {stdout, stderr, status} = await runCommand(
            [bin, 'json', '--help'],
            {timeoutMs: TOKENS_PROBE_TIMEOUT_MS},
        );
        if (status === 0 && /--tokens\b/.test(`${stdout}\n${stderr}`))
            return ['json', '--tokens'];
    } catch (_error) {
        // fall through
    }

    return null;
}

async function tokensArgv(bin) {
    if (_tokensCommand !== undefined && _tokensCommand.bin === bin)
        return _tokensCommand.argv;

    let argv = readCachedTokensArgv(bin);
    if (argv === undefined) {
        argv = await detectTokensArgv(bin);
        writeCachedTokensArgv(bin, argv);
    }

    _tokensCommand = {bin, argv};
    return argv;
}

/**
 * Fetch the token ledger via `usg tokens --json` (or `usg json --tokens`).
 * Returns null — never throws — when the CLI is missing, lacks the tokens
 * surface, errors out, or the ledger is empty, so callers can hide the row.
 * @param {{binaryPath?: string|null}} [options]
 * @returns {Promise<import('./types.js').TokenLedger|null>}
 */
export async function fetchTokens({binaryPath = resolveCliBinary()} = {}) {
    const bin = binaryPath;
    if (!bin)
        return null;

    const argv = await tokensArgv(bin);
    if (!argv)
        return null;

    try {
        const {stdout, status} = await runCommand(
            [bin, ...argv],
            {timeoutMs: TOKENS_TIMEOUT_MS},
        );
        if (status !== 0)
            return null;
        return normalizeTokenLedger(JSON.parse(stdout.trim() || 'null'));
    } catch (_error) {
        return null;
    }
}

/**
 * Connection test via `usg test -q -p <id>`.
 * @param {string} providerId
 * @returns {Promise<{ok: boolean, message: string}>}
 */
export async function testProviderFromCli(providerId) {
    const bin = resolveCliBinary();
    if (!bin)
        return {ok: false, message: cliMissingMessage()};

    try {
        const {stdout, stderr, status} = await runCli(
            ['test', '-q', '-p', providerId],
            {timeoutMs: TEST_TIMEOUT_MS},
        );
        const text = (stdout || stderr || '').trim();
        const message = extractTestMessage(text, providerId)
            || (status === 0 ? 'Connected' : `Failed (exit ${status})`);
        return {ok: status === 0, message};
    } catch (error) {
        return {
            ok: false,
            message: error instanceof Error ? error.message : String(error),
        };
    }
}

/**
 * @param {string} text
 * @param {string} providerId
 */
function extractTestMessage(text, providerId) {
    if (!text)
        return null;
    const lines = text.split('\n').map(l => l.trim()).filter(Boolean);
    const needle = providerId.toLowerCase();
    const match = lines.find(l => l.toLowerCase().includes(needle)) ?? lines[lines.length - 1];
    if (!match)
        return null;
    // "Codex        Connected as …" → drop leading label column when present
    const stripped = match.replace(/^[✓✗!\s]+/, '');
    const parts = stripped.split(/\s{2,}/);
    if (parts.length >= 2 && parts[0].toLowerCase().includes(needle))
        return parts.slice(1).join(' ').trim() || stripped;
    return stripped;
}

function titleCase(id) {
    return id.charAt(0).toUpperCase() + id.slice(1);
}
