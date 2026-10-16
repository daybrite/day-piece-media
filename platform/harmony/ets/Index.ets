// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// day-piece-media: the HarmonyOS half of the piece (docs/extending.md, docs/media.md).
//
// The ArkUI C node API has no video node kind, so the player is declared here in ArkTS: a `Video`
// component for pictures (built into a BuilderNode and handed to Day as a FrameNode the native
// tree mounts like any other node), or an `AVPlayer` from @kit.MediaKit for sound only, behind an
// empty Column of no size. `day build` stages this file into the app's hvigor project and
// generates the aggregator that registers `dayPiece` with the shim.
//
// Commands arrive as the piece's own vocabulary ('load' | 'play' | 'pause' | 'stop' | 'volume').
// Playback state goes back through `pieceEvent` with the piece's report codes as `num` (0 idle,
// 1 loading, 2 playing, 3 paused, 4 ended, 5 error), which reaches the Rust front-end as an
// `Event::Custom`, the same channel the Android renderer uses.

import { BuilderNode, FrameNode, UIContext } from '@kit.ArkUI';
import { media } from '@kit.MediaKit';
import { fileIo } from '@kit.CoreFileKit';
import { avSession } from '@kit.AVSessionKit';
import { common, wantAgent } from '@kit.AbilityKit';
import { backgroundTaskManager } from '@kit.BackgroundTasksKit';
import { BusinessError } from '@kit.BasicServicesKit';
import nativeEntry from 'libentry.so';
import { DayPieceModule } from '../DayPiece';

const IDLE: number = 0;
const LOADING: number = 1;
const PLAYING: number = 2;
const PAUSED: number = 3;
const ENDED: number = 4;
const ERROR: number = 5;

function report(id: number, code: number, text: string = ''): void {
  nativeEntry.pieceEvent(id, text, code);
  const live: DayMediaInstance | undefined = instances.get(id);
  if (live !== undefined && code <= ERROR) {
    live.active = code === PLAYING || code === LOADING;
    refreshBackground();
  }
}

// What the props string carries, 0x1F-separated in this order (lib-arkui.rs composes it).
class DayMediaParams {
  id: number = 0;
  url: string = '';
  autoplay: boolean = true;
  looping: boolean = false;
  muted: boolean = false;
  controls: boolean = true;
  audioOnly: boolean = false;
  volume: number = 1.0;
  rate: number = 1.0;
  position: number = 0;
  duration: number = 0;
  controller: VideoController = new VideoController();
}

function parseParams(id: number, props: string): DayMediaParams {
  const p: DayMediaParams = new DayMediaParams();
  p.id = id;
  const parts: string[] = props.split('\u001F');
  p.url = parts.length > 0 ? parts[0] : '';
  p.autoplay = parts.length > 1 ? parts[1] === '1' : true;
  p.looping = parts.length > 2 ? parts[2] === '1' : false;
  p.muted = parts.length > 3 ? parts[3] === '1' : false;
  p.controls = parts.length > 4 ? parts[4] === '1' : true;
  p.audioOnly = parts.length > 5 ? parts[5] === '1' : false;
  const v: number = parts.length > 6 ? Number(parts[6]) : 1.0;
  p.volume = Number.isNaN(v) ? 1.0 : v;
  return p;
}

// One live player, either shape. Held until Day disposes the node.
class DayMediaInstance {
  node: BuilderNode<[DayMediaParams]>;
  params: DayMediaParams;
  // The sound-only engine; a Video component drives its own.
  player: media.AVPlayer | null = null;
  file: fileIo.File | null = null;
  appliedRate: number = 1;
  // Whether the app asked to play before the source was ready.
  playWhenReady: boolean = false;
  generation: number = 0;
  background: boolean = false;
  active: boolean = false;
  context: common.UIAbilityContext;

  constructor(node: BuilderNode<[DayMediaParams]>, params: DayMediaParams, context: common.UIAbilityContext) {
    this.context = context;
    this.node = node;
    this.params = params;
  }
}

// The picture player. NO percentage sizing: a BuilderNode is built detached, so '100%' resolves
// against the whole window. Day owns layout and sets this node's position + size through the
// ArkUI C API like every other leaf, which is what the piece's `fill_measure` asks for.
@Builder
function dayVideoBuilder(params: DayMediaParams) {
  Video({ src: params.url, controller: params.controller, currentProgressRate: params.rate })
    .autoPlay(params.autoplay)
    .loop(params.looping)
    .muted(params.muted)
    .controls(params.controls)
    .onPrepared((event) => { if (event) { params.duration = event.duration; } report(params.id, params.autoplay ? PLAYING : PAUSED); })
    .onUpdate((event) => { if (event) { params.position = event.time; } })
    .onStart(() => { report(params.id, PLAYING); })
    .onPause(() => { report(params.id, PAUSED); })
    .onFinish(() => { if (!params.looping) { report(params.id, ENDED); } })
    .onStop(() => { report(params.id, IDLE); })
    .onError(() => { report(params.id, ERROR, 'playback failed'); })
}

// The sound-only leaf: nothing to draw, so an empty container Day measures at zero.
@Builder
function dayAudioBuilder(params: DayMediaParams) {
  Column()
    .width(0)
    .height(0)
}

const instances: Map<number, DayMediaInstance> = new Map();

// One continuous task per ability, shared by opted-in audio sessions. Serialize async
// start/stop so source replacement cannot leave an orphaned task or AVSession.
let backgroundContext: common.UIAbilityContext | null = null;
let systemSession: avSession.AVSession | null = null;
let backgroundWork: Promise<void> = Promise.resolve();
function refreshBackground(): void {
  backgroundWork = backgroundWork.then(async () => {
    const candidates: DayMediaInstance[] = [];
    instances.forEach((live: DayMediaInstance) => {
      if (live.params.audioOnly && live.background && live.active) { candidates.push(live); }
    });
    const selected: DayMediaInstance | undefined = candidates[0];
    if (selected !== undefined && backgroundContext === selected.context) { return; }
    if (backgroundContext !== null) {
      const old: common.UIAbilityContext = backgroundContext;
      backgroundContext = null;
      await backgroundTaskManager.stopBackgroundRunning(old);
    }
    if (systemSession !== null) { await systemSession.destroy(); systemSession = null; }
    if (selected === undefined) { return; }
    try {
      const context: common.UIAbilityContext = selected.context;
      systemSession = await avSession.createAVSession(context, 'day.media', 'audio');
      await systemSession.activate();
      const agent = await wantAgent.getWantAgent({
        wants: [{ bundleName: context.abilityInfo.bundleName, abilityName: context.abilityInfo.name }],
        operationType: wantAgent.OperationType.START_ABILITY,
        requestCode: 0,
        wantAgentFlags: [wantAgent.WantAgentFlags.UPDATE_PRESENT_FLAG]
      });
      await backgroundTaskManager.startBackgroundRunning(context, backgroundTaskManager.BackgroundMode.AUDIO_PLAYBACK, agent);
      backgroundContext = context;
    } catch (err) {
      if (systemSession !== null) { await systemSession.destroy(); systemSession = null; }
      report(selected.params.id, 8, 'background:failed');
    }
  }).catch((err: BusinessError) => { console.warn(`day-piece-media background: ${err.message}`); });
}

// Build the AVPlayer for `url` and start it when it reports ready. The player walks a state
// machine (idle → initialized → prepared → playing …); each step is a `stateChange` event, which
// is also where the piece's own report comes from.
function loadAudio(live: DayMediaInstance, url: string, playWhenReady: boolean): void {
  releaseAudio(live);
  const generation: number = live.generation;
  live.params.position = 0;
  live.params.duration = 0;
  live.playWhenReady = playWhenReady;
  report(live.params.id, LOADING);
  media.createAVPlayer().then(async (player: media.AVPlayer) => {
    if (instances.get(live.params.id) !== live || generation !== live.generation) {
      player.release();
      return;
    }
    live.player = player;
    player.on('stateChange', (state: string) => {
      if (live.player !== player) {
        return;
      }
      if (state === 'initialized') {
        player.loop = live.params.looping;
        player.prepare().catch((err: BusinessError) => {
          report(live.params.id, ERROR, String(err.message));
        });
      } else if (state === 'prepared') {
        player.setVolume(live.params.muted ? 0 : live.params.volume);
        applySpeed(live);
        live.params.duration = player.duration / 1000;
        if (live.playWhenReady) {
          player.play();
        } else {
          report(live.params.id, PAUSED);
        }
      } else if (state === 'playing') {
        report(live.params.id, PLAYING);
      } else if (state === 'paused') {
        report(live.params.id, PAUSED);
      } else if (state === 'completed') {
        if (!live.params.looping) {
          report(live.params.id, ENDED);
        }
      } else if (state === 'stopped' || state === 'idle') {
        report(live.params.id, IDLE);
      } else if (state === 'error') {
        report(live.params.id, ERROR, 'playback failed');
      }
    });
    player.on('speedDone', (speed: number) => {
      if (live.player !== player) { return; }
      switch (speed) {
        case media.PlaybackSpeed.SPEED_FORWARD_0_75_X: live.appliedRate = 0.75; break;
        case media.PlaybackSpeed.SPEED_FORWARD_1_00_X: live.appliedRate = 1; break;
        case media.PlaybackSpeed.SPEED_FORWARD_1_25_X: live.appliedRate = 1.25; break;
        case media.PlaybackSpeed.SPEED_FORWARD_1_75_X: live.appliedRate = 1.75; break;
        case media.PlaybackSpeed.SPEED_FORWARD_2_00_X: live.appliedRate = 2; break;
      }
    });
    player.on('timeUpdate', (time: number) => { if (live.player === player) { live.params.position = time / 1000; } });
    player.on('error', (err: BusinessError) => {
      if (live.player === player) {
        report(live.params.id, ERROR, String(err.message));
      }
    });
    // A stalled network shows up as buffering updates rather than a state change.
    player.on('bufferingUpdate', (infoType: media.BufferingInfoType) => {
      if (live.player !== player) {
        return;
      }
      if (infoType === media.BufferingInfoType.BUFFERING_START) {
        report(live.params.id, LOADING);
      } else if (infoType === media.BufferingInfoType.BUFFERING_END) {
        report(live.params.id, player.state === 'playing' ? PLAYING : PAUSED);
      }
    });
    // Setting the url moves the player to `initialized`, which the handler above continues.
    if (url.startsWith('/') || url.startsWith('file://')) {
      const file: fileIo.File = await fileIo.open(url, fileIo.OpenMode.READ_ONLY);
      if (live.player !== player || generation !== live.generation) { fileIo.closeSync(file); return; }
      live.file = file;
      player.fdSrc = { fd: file.fd };
    } else { player.url = url; }
  }).catch((err: BusinessError) => {
    if (generation === live.generation && instances.get(live.params.id) === live) { report(live.params.id, ERROR, String(err.message)); }
  });
}

function releaseAudio(live: DayMediaInstance): void {
  live.generation++;
  live.active = false;
  refreshBackground();
  const old: media.AVPlayer | null = live.player;
  const file: fileIo.File | null = live.file;
  live.file = null;
  live.player = null;
  if (old !== null) {
    try {
      old.release().finally(() => { if (file !== null) { fileIo.closeSync(file); } })
        .catch((err: BusinessError) => { console.warn(`day-piece-media release: ${err.message}`); });
    } catch (err) {
      if (file !== null) { fileIo.closeSync(file); }
      console.warn(`day-piece-media: release failed: ${err}`);
    }
  }
}

function make(ui: UIContext, id: number, props: string): FrameNode | undefined {
  const params: DayMediaParams = parseParams(id, props);
  const node: BuilderNode<[DayMediaParams]> = new BuilderNode(ui);
  if (params.audioOnly) {
    node.build(wrapBuilder<[DayMediaParams]>(dayAudioBuilder), params);
  } else {
    node.build(wrapBuilder<[DayMediaParams]>(dayVideoBuilder), params);
  }
  const frame: FrameNode | null = node.getFrameNode();
  if (frame === null) {
    return undefined;
  }
  const live: DayMediaInstance = new DayMediaInstance(node, params, ui.getHostContext() as common.UIAbilityContext);
  instances.set(id, live);
  if (params.audioOnly && params.url !== '') {
    loadAudio(live, params.url, params.autoplay);
  }
  return frame;
}

function applySpeed(live: DayMediaInstance): void {
  const player: media.AVPlayer | null = live.player;
  if (player === null) { return; }
  let speed: media.PlaybackSpeed = media.PlaybackSpeed.SPEED_FORWARD_1_00_X;
  switch (live.params.rate) {
    case 0.75: speed = media.PlaybackSpeed.SPEED_FORWARD_0_75_X; break;
    case 1: break;
    case 1.25: speed = media.PlaybackSpeed.SPEED_FORWARD_1_25_X; break;
    case 1.75: speed = media.PlaybackSpeed.SPEED_FORWARD_1_75_X; break;
    case 2: speed = media.PlaybackSpeed.SPEED_FORWARD_2_00_X; break;
    default: report(live.params.id, 8, 'rate:unsupported'); return;
  }
  player.setSpeed(speed);
}

class TimelineRange {
  start: number = 0;
  end: number;
  constructor(end: number) { this.end = end; }
}
class TimelineCapabilities {
  seek: boolean = false;
  rate: boolean = true;
  volume: boolean = false;
}
class TimelineReport {
  position: number = 0;
  duration: number | null = null;
  rate: number = 1;
  seekable: TimelineRange[] = [];
  capabilities: TimelineCapabilities = new TimelineCapabilities();
}
function poll(live: DayMediaInstance): void {
  const value: TimelineReport = new TimelineReport();
  value.position = live.params.position;
  value.duration = live.params.duration > 0 ? live.params.duration : null;
  value.rate = live.params.audioOnly ? live.appliedRate : live.params.rate;
  value.capabilities.seek = live.params.duration > 0;
  value.capabilities.volume = live.params.audioOnly;
  if (live.params.duration > 0) { value.seekable.push(new TimelineRange(live.params.duration)); }
  report(live.params.id, 7, JSON.stringify(value));
}

function update(id: number, cmd: string, arg: string): void {
  const live: DayMediaInstance | undefined = instances.get(id);
  if (live === undefined) {
    return;
  }
  if (cmd === 'background') { live.background = true; refreshBackground(); return; }
  if (cmd === 'poll') { poll(live); return; }
  if (cmd === 'seek') {
    const seconds: number = Number(arg);
    if (!Number.isFinite(seconds) || seconds < 0 || live.params.duration <= 0) { report(id, 8, 'seek:unsupported'); return; }
    const target: number = Math.min(seconds, live.params.duration);
    try {
      if (live.params.audioOnly && live.player !== null) { live.player.seek(Math.round(target * 1000)); }
      else { live.params.controller.setCurrentTime(target, SeekMode.Accurate); }
    } catch (err) { report(id, 8, 'seek:failed'); }
    return;
  }
  if (cmd === 'rate') {
    const rate: number = Number(arg);
    if ([0.75, 1, 1.25, 1.75, 2].indexOf(rate) < 0) { report(id, 8, 'rate:unsupported'); return; }
    live.params.rate = rate;
    try {
      if (live.params.audioOnly) {
        if (live.player !== null && ['prepared', 'playing', 'paused', 'completed'].indexOf(live.player.state) >= 0) { applySpeed(live); }
      } else { live.node.update(live.params); }
    } catch (err) { report(id, 8, 'rate:failed'); }
    return;
  }
  if (live.params.audioOnly) {
    const player: media.AVPlayer | null = live.player;
    try {
      if (cmd === 'load') {
        loadAudio(live, arg, true);
      } else if (cmd === 'play') {
        live.playWhenReady = true;
        if (player !== null && (player.state === 'prepared' || player.state === 'paused'
          || player.state === 'completed' || player.state === 'stopped')) {
          if (player.state === 'stopped') {
            player.prepare();
          } else {
            player.play();
          }
        }
      } else if (cmd === 'pause') {
        live.playWhenReady = false;
        if (player !== null && player.state === 'playing') {
          player.pause();
        }
      } else if (cmd === 'stop') {
        // Dropping the player is what lets a live stream's connection go.
        releaseAudio(live);
        report(id, IDLE);
      } else if (cmd === 'volume') {
        const v: number = Number(arg);
        if (!Number.isNaN(v)) {
          live.params.volume = v;
          if (player !== null && !live.params.muted) {
            player.setVolume(v);
          }
        }
      }
    } catch (err) {
      console.error(`day-piece-media: ${cmd} failed: ${err}`);
    }
    return;
  }
  // The picture player: driven through its VideoController. A `load` swaps the source by
  // rebuilding the component (the Video's src is fixed at build), which is also what a `stop`
  // needs to let the connection go.
  try {
    if (cmd === 'load') {
      live.params.url = arg;
      live.params.autoplay = true;
      live.node.update(live.params);
      report(id, LOADING);
    } else if (cmd === 'play') {
      live.params.controller.start();
    } else if (cmd === 'pause') {
      live.params.controller.pause();
    } else if (cmd === 'stop') {
      live.params.controller.stop();
      report(id, IDLE);
    } else if (cmd === 'volume') {
      // The Video component exposes mute but no volume; a muted player stays muted.
      const v: number = Number(arg);
      if (!Number.isNaN(v)) {
        live.params.volume = v;
      }
    }
  } catch (err) {
    console.error(`day-piece-media: ${cmd} failed: ${err}`);
  }
}

function dispose(id: number): void {
  const live: DayMediaInstance | undefined = instances.get(id);
  if (live !== undefined) {
    instances.delete(id);
    releaseAudio(live);
    live.node.dispose();
  }
}

export const dayPiece: DayPieceModule = {
  kind: 'day.piece.media',
  make: make,
  update: update,
  dispose: dispose
};
