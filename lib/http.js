import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Soup from 'gi://Soup';

Gio._promisify(Soup.Session.prototype, 'send_and_read_async', 'send_and_read_finish');

export class HttpError extends Error {
    constructor(message, {statusCode = 0, body = null} = {}) {
        super(String(message));
        this.name = 'HttpError';
        this.statusCode = statusCode;
        this.body = body;
    }

    get isAuthError() {
        return this.statusCode === 401 || this.statusCode === 403;
    }
}

export class HttpClient {
    constructor({timeout = 30} = {}) {
        this._session = new Soup.Session({timeout});
    }

    destroy() {
        this._session.abort();
    }

    async getJson(url, {headers = {}} = {}) {
        return this._requestJson('GET', url, {headers});
    }

    async postJson(url, body, {headers = {}} = {}) {
        return this._requestJson('POST', url, {
            headers: {
                'Content-Type': 'application/json',
                ...headers,
            },
            body: typeof body === 'string' ? body : JSON.stringify(body ?? {}),
        });
    }

    async postForm(url, fields, {headers = {}} = {}) {
        const form = Object.entries(fields)
            .map(([key, value]) =>
                `${encodeURIComponent(key)}=${encodeURIComponent(String(value ?? ''))}`)
            .join('&');

        return this._requestJson('POST', url, {
            headers: {
                'Content-Type': 'application/x-www-form-urlencoded',
                ...headers,
            },
            body: form,
        });
    }

    async _requestJson(method, url, {headers = {}, body = null} = {}) {
        const message = Soup.Message.new(method, url);
        const requestHeaders = message.get_request_headers();
        for (const [key, value] of Object.entries(headers)) {
            if (value !== undefined && value !== null && value !== '')
                requestHeaders.append(key, String(value));
        }

        if (body !== null) {
            const bytes = new TextEncoder().encode(body);
            message.set_request_body_from_bytes(
                headers['Content-Type'] ?? 'application/octet-stream',
                GLib.Bytes.new(bytes),
            );
        }

        const responseBytes = await this._session.send_and_read_async(
            message,
            GLib.PRIORITY_DEFAULT,
            null,
        );

        const statusCode = message.get_status();
        const text = decodeBytes(responseBytes);
        let payload = null;
        if (text) {
            try {
                payload = JSON.parse(text);
            } catch (error) {
                if (statusCode >= 200 && statusCode < 300) {
                    throw new HttpError(`Invalid JSON from ${url}`, {
                        statusCode,
                        body: text,
                    });
                }
            }
        }

        if (statusCode < 200 || statusCode >= 300) {
            const messageText = extractErrorMessage(payload) ||
                `Request failed with HTTP ${statusCode}.`;
            throw new HttpError(messageText, {statusCode, body: payload ?? text});
        }

        return payload;
    }
}

export function decodeBytes(bytes) {
    const data = bytes?.toArray?.() ?? bytes?.get_data?.() ?? [];
    return new TextDecoder().decode(data);
}

function extractErrorMessage(payload) {
    if (!payload)
        return '';
    if (typeof payload === 'string')
        return payload.trim();
    if (typeof payload !== 'object')
        return '';

    for (const value of [payload.message, payload.error, payload.detail, payload.title, payload.description]) {
        if (typeof value === 'string' && value.trim())
            return value.trim();
        if (value && typeof value === 'object') {
            if (typeof value.message === 'string' && value.message.trim())
                return value.message.trim();
        }
    }
    return '';
}
