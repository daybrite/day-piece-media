// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
const source = readFileSync(new URL('../src/browser.rs', import.meta.url), 'utf8');
const arm = source.match(/js!\(r#"([\s\S]*?)"#\);/)[1];
function setup() {
    const element = new EventTarget();
    const reports = [];
    let release;
    element.pause = () => { element.paused = true; element.dispatchEvent(new Event('pause')); };
    element.load = () => { element.loaded = true; element.dispatchEvent(new Event('emptied')); };
    element.removeAttribute = name => { element.removed = name; };
    const bridge = runInNewContext(`${arm}\n({attach_media, set_volume})`, {
        dayHost: { dom: {
            element: () => element,
            emit: (...args) => reports.push(args),
            onRelease: (_id, cleanup) => { release = cleanup; },
        } },
    });
    bridge.attach_media(3, 0.5);
    return { element, reports, bridge, release: () => release() };
}
test('playback events carry the native state protocol', () => {
    const { element, reports } = setup();
    for (const [event, code] of [['loadstart',1],['waiting',1],['playing',2],['pause',3],['ended',4],['emptied',0]]) {
        element.dispatchEvent(new Event(event));
        assert.deepEqual(reports.at(-1), [3, code, '']);
    }
    element.ended = true;
    const count = reports.length;
    element.dispatchEvent(new Event('pause'));
    assert.equal(reports.length, count);
});
test('errors distinguish deliberate aborts from playback failures', () => {
    const { element, reports } = setup();
    element.error = { code: 1 };
    element.dispatchEvent(new Event('error'));
    assert.equal(reports.length, 0);
    element.error = { code: 4 };
    element.dispatchEvent(new Event('error'));
    assert.deepEqual(reports[0], [3, 5, 'stream format not supported by this browser']);
});
test('volume is a clamped property and release silences callbacks before unloading', () => {
    const { element, reports, bridge, release } = setup();
    assert.equal(element.volume, 0.5);
    bridge.set_volume(3, 2); assert.equal(element.volume, 1);
    bridge.set_volume(3, -1); assert.equal(element.volume, 0);
    bridge.set_volume(3, NaN); assert.equal(element.volume, 0);
    release();
    element.dispatchEvent(new Event('playing'));
    assert.equal(reports.length, 0);
    assert.equal(element.paused, true);
    assert.equal(element.loaded, true);
    assert.equal(element.removed, 'src');
});
