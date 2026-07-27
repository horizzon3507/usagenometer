import {fetchAntigravitySnapshot} from '../providers/antigravity/index.js';
import {fetchCodexSnapshot} from '../providers/codex/index.js';
import {fetchCursorSnapshot} from '../providers/cursor/index.js';

const results = await Promise.all([
    fetchCursorSnapshot(),
    fetchAntigravitySnapshot(),
    fetchCodexSnapshot(),
]);

for (const snapshot of results) {
    print(JSON.stringify({
        id: snapshot.id,
        status: snapshot.status,
        account: snapshot.account,
        plan: snapshot.plan,
        meters: snapshot.meters.map(meter => ({
            id: meter.id,
            title: meter.title,
            percent: meter.percent,
            leftPercent: meter.leftPercent,
        })),
        error: snapshot.error,
    }));
}
