import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

Gio._promisify(Gio.Subprocess.prototype, 'communicate_utf8_async', 'communicate_utf8_finish');

/**
 * Run a short-lived command and return stdout (utf-8).
 * @param {string[]} argv
 * @param {{timeoutMs?: number}} [options]
 * @returns {Promise<{stdout: string, stderr: string, status: number}>}
 */
export async function runCommand(argv, {timeoutMs = 15000} = {}) {
    const proc = Gio.Subprocess.new(
        argv,
        Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_PIPE,
    );

    let timedOut = false;
    let timeoutId = 0;
    if (timeoutMs > 0) {
        timeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, timeoutMs, () => {
            timedOut = true;
            try {
                proc.force_exit();
            } catch (error) {
                // ignore
            }
            return GLib.SOURCE_REMOVE;
        });
    }

    try {
        // GJS promisify returns [stdout, stderr] (boolean success is not included).
        const [stdout, stderr] = await proc.communicate_utf8_async(null, null);
        if (timedOut)
            throw new Error(`Command timed out: ${argv.join(' ')}`);

        return {
            stdout: stdout ?? '',
            stderr: stderr ?? '',
            status: proc.get_exit_status(),
        };
    } finally {
        if (timeoutId)
            GLib.Source.remove(timeoutId);
    }
}
