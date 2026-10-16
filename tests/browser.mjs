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
    const bridge = runInNewContext(`${arm}\n({attach_media, set_volume, stop_media, seek_media, rate_media, poll_media, play_media})`, {
        dayHost: { dom: {
            element: () => element,
            emit: (...args) => reports.push(args),
            onRelease: (_id, cleanup) => { release = cleanup; },
        } },
    });
    bridge.attach_media(3, 0.5, false);
    return { element, reports, bridge, release: () => release() };
}
test('muting is the property, which a script-set attribute never reaches', () => {
    const { element, bridge } = setup();
    assert.equal(element.muted, false);
    bridge.attach_media(3, 0.5, true);
    assert.equal(element.muted, true);
});
test('stop removes the source instead of emptying it, and ends on Idle', () => {
    const { element, reports, bridge } = setup();
    bridge.stop_media(3);
    assert.equal(element.paused, true);
    assert.equal(element.removed, 'src');
    assert.equal(element.loaded, true);
    assert.deepEqual(reports.map(([, code]) => code), [3, 0]);
});
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

const ranges = values => ({length: values.length, start: i => values[i][0], end: i => values[i][1]});
test('timeline preserves live ranges and unknown duration', () => {
    const {element, reports, bridge} = setup();
    element.currentTime = 42; element.duration = Infinity; element.playbackRate = 1.25;
    element.seekable = ranges([[20,30],[40,60]]); element.buffered = ranges([[41,45]]);
    bridge.poll_media(3);
    const timeline = JSON.parse(reports.at(-1)[2]);
    assert.equal(timeline.duration, null);
    assert.equal(timeline.position, 42);
    assert.equal(timeline.rate, 1.25);
    assert.equal(timeline.seekable.length, 2);
    assert.deepEqual(timeline.buffered, [{start:41,end:45}]);
});
test('seek clamps into available windows without starting paused playback', () => {
    const {element, reports, bridge} = setup();
    element.paused = true; element.seekable = ranges([[20,30],[40,60]]);
    bridge.seek_media(3, 38); assert.equal(element.currentTime,40); assert.equal(element.paused,true);
    bridge.seek_media(3, 900); assert.equal(element.currentTime,60);
    bridge.seek_media(3, NaN); assert.equal(element.currentTime,60);
    element.seekable = ranges([]);
    bridge.seek_media(3, 10); assert.deepEqual(reports.at(-1),[3,8,'seek:unsupported']);
});
test('rate is persistent across play and pause; rejected autoplay is observable', async () => {
    const {element,reports,bridge} = setup();
    element.paused = true;
    bridge.rate_media(3,1.25);
    assert.equal(element.playbackRate,1.25); assert.equal(element.defaultPlaybackRate,1.25);
    assert.equal(element.paused,true);
    bridge.rate_media(3,0); assert.equal(element.playbackRate,1.25);
    element.play = () => Promise.reject({name:'NotAllowedError'});
    bridge.play_media(3); await Promise.resolve();
    assert.deepEqual(reports.at(-1),[3,8,'play:NotAllowedError']);
});
