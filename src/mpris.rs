// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! MPRIS (`org.mpris.MediaPlayer2`) on the Linux session bus: the now-playing surface GNOME's
//! shell, KDE's media controller, `playerctl` and the desktop's media keys read and drive. The
//! GTK and Qt arms share it; macOS has no session bus by default, so it is Linux-only.
//!
//! One service per process, started by the first [`publish`]. It owns
//! `org.mpris.MediaPlayer2.<name>` (the app id's last element, else the executable's name) and
//! exports `/org/mpris/MediaPlayer2` with the root and `Player` interfaces and
//! `org.freedesktop.DBus.Properties`. Updates come from the UI thread over a channel; the
//! service thread turns each into a `PropertiesChanged` signal for whatever changed, and a
//! `Seeked` signal when the position jumped. Calls from clients arrive on the connection's
//! reader thread and go back to the UI thread as [`report::REMOTE`](crate::report::REMOTE)
//! events on the node that published last, through the emitter the arm [`install`]ed.
//!
//! Without a session bus the first publish logs once at debug level and the rest are dropped.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc};
use std::time::Instant;

use day_platform_linux::dbus::{
    Connection, INTROSPECTABLE_INTERFACE, MethodCall, MethodError, NAME_FLAG_DO_NOT_QUEUE,
    NAME_REPLY_ALREADY_OWNER, NAME_REPLY_EXISTS, NAME_REPLY_PRIMARY_OWNER, PROPERTIES_INTERFACE,
    Value, introspection_xml,
};
use day_spec::NodeId;

use crate::NowPlayingInfo;

/// The one object path the specification fixes.
const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_INTERFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
/// A position that moved further than this between two publishes was a seek, not playback.
const SEEK_JUMP_SECS: f64 = 2.0;

const ROOT_PROPERTIES: &[&str] = &[
    "CanQuit",
    "CanRaise",
    "HasTrackList",
    "Identity",
    "DesktopEntry",
    "SupportedUriSchemes",
    "SupportedMimeTypes",
];
const PLAYER_PROPERTIES: &[&str] = &[
    "PlaybackStatus",
    "Rate",
    "Metadata",
    "Volume",
    "Position",
    "MinimumRate",
    "MaximumRate",
    "CanGoNext",
    "CanGoPrevious",
    "CanPlay",
    "CanPause",
    "CanSeek",
    "CanControl",
];

const XML: &str = " <interface name=\"org.freedesktop.DBus.Properties\">\n  \
    <method name=\"Get\"><arg type=\"s\" name=\"interface_name\" direction=\"in\"/><arg type=\"s\" name=\"property_name\" direction=\"in\"/><arg type=\"v\" name=\"value\" direction=\"out\"/></method>\n  \
    <method name=\"GetAll\"><arg type=\"s\" name=\"interface_name\" direction=\"in\"/><arg type=\"a{sv}\" name=\"properties\" direction=\"out\"/></method>\n  \
    <method name=\"Set\"><arg type=\"s\" name=\"interface_name\" direction=\"in\"/><arg type=\"s\" name=\"property_name\" direction=\"in\"/><arg type=\"v\" name=\"value\" direction=\"in\"/></method>\n  \
    <signal name=\"PropertiesChanged\"><arg type=\"s\" name=\"interface_name\"/><arg type=\"a{sv}\" name=\"changed_properties\"/><arg type=\"as\" name=\"invalidated_properties\"/></signal>\n \
    </interface>\n \
    <interface name=\"org.mpris.MediaPlayer2\">\n  \
    <method name=\"Raise\"/>\n  \
    <method name=\"Quit\"/>\n  \
    <property name=\"CanQuit\" type=\"b\" access=\"read\"/>\n  \
    <property name=\"CanRaise\" type=\"b\" access=\"read\"/>\n  \
    <property name=\"HasTrackList\" type=\"b\" access=\"read\"/>\n  \
    <property name=\"Identity\" type=\"s\" access=\"read\"/>\n  \
    <property name=\"DesktopEntry\" type=\"s\" access=\"read\"/>\n  \
    <property name=\"SupportedUriSchemes\" type=\"as\" access=\"read\"/>\n  \
    <property name=\"SupportedMimeTypes\" type=\"as\" access=\"read\"/>\n \
    </interface>\n \
    <interface name=\"org.mpris.MediaPlayer2.Player\">\n  \
    <method name=\"Next\"/>\n  \
    <method name=\"Previous\"/>\n  \
    <method name=\"Pause\"/>\n  \
    <method name=\"PlayPause\"/>\n  \
    <method name=\"Stop\"/>\n  \
    <method name=\"Play\"/>\n  \
    <method name=\"Seek\"><arg type=\"x\" name=\"Offset\" direction=\"in\"/></method>\n  \
    <method name=\"SetPosition\"><arg type=\"o\" name=\"TrackId\" direction=\"in\"/><arg type=\"x\" name=\"Position\" direction=\"in\"/></method>\n  \
    <method name=\"OpenUri\"><arg type=\"s\" name=\"Uri\" direction=\"in\"/></method>\n  \
    <signal name=\"Seeked\"><arg type=\"x\" name=\"Position\"/></signal>\n  \
    <property name=\"PlaybackStatus\" type=\"s\" access=\"read\"/>\n  \
    <property name=\"Rate\" type=\"d\" access=\"readwrite\"/>\n  \
    <property name=\"Metadata\" type=\"a{sv}\" access=\"read\"/>\n  \
    <property name=\"Volume\" type=\"d\" access=\"readwrite\"/>\n  \
    <property name=\"Position\" type=\"x\" access=\"read\"/>\n  \
    <property name=\"MinimumRate\" type=\"d\" access=\"read\"/>\n  \
    <property name=\"MaximumRate\" type=\"d\" access=\"read\"/>\n  \
    <property name=\"CanGoNext\" type=\"b\" access=\"read\"/>\n  \
    <property name=\"CanGoPrevious\" type=\"b\" access=\"read\"/>\n  \
    <property name=\"CanPlay\" type=\"b\" access=\"read\"/>\n  \
    <property name=\"CanPause\" type=\"b\" access=\"read\"/>\n  \
    <property name=\"CanSeek\" type=\"b\" access=\"read\"/>\n  \
    <property name=\"CanControl\" type=\"b\" access=\"read\"/>\n \
    </interface>\n";

/// How a client's command reaches the app: the arm's emit of a `REMOTE` report, run on the UI
/// thread with the node that published last and the command's wire form.
type Emit = fn(NodeId, String);

static EMIT: OnceLock<Emit> = OnceLock::new();
/// The channel into the service thread; `None` when the thread could not be started.
static SERVICE: OnceLock<Option<mpsc::Sender<Update>>> = OnceLock::new();

struct Update {
    node: NodeId,
    info: Option<NowPlayingInfo>,
}

/// Registers the arm's report emitter. The first registration wins; the arms register the same
/// function on every publish, which is the cheap way to be sure one is in place.
pub(crate) fn install(emit: Emit) {
    let _ = EMIT.set(emit);
}

/// Publishes what `node`'s player has on (`None` clears it), starting the service on first use.
pub(crate) fn publish(node: NodeId, info: Option<NowPlayingInfo>) {
    let service = SERVICE.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new()
            .name("day-media-mpris".into())
            .spawn(move || serve(rx))
        {
            Ok(_) => Some(tx),
            Err(e) => {
                log::debug!("media: MPRIS service thread not started: {e}");
                None
            }
        }
    });
    if let Some(tx) = service {
        // A failed send means the service gave up (no bus); there is nobody to tell.
        let _ = tx.send(Update { node, info });
    }
}

/// What the service answers from, shared between the service thread (writing) and the
/// connection's reader thread (answering calls).
struct State {
    identity: String,
    /// The app id, which is the desktop id on Linux (`DAY_APP_ID`), so a shell can find the icon.
    desktop_entry: Option<String>,
    node: Option<NodeId>,
    info: Option<NowPlayingInfo>,
    /// When `info` was published; a playing position runs on from there (see [`State::position`]).
    at: Instant,
}

impl State {
    fn new() -> State {
        let app_id = env("DAY_APP_ID");
        let identity = env("DAY_APP_TITLE")
            .or_else(|| app_id.as_deref().map(|id| last_element(id).to_owned()))
            .unwrap_or_else(player_name);
        State {
            identity,
            desktop_entry: app_id,
            node: None,
            info: None,
            at: Instant::now(),
        }
    }

    /// The position now, in seconds: the published one, advanced by the time since while
    /// playing, and never past the end.
    fn position(&self) -> f64 {
        let Some(i) = &self.info else {
            return 0.0;
        };
        let mut p = if i.position.is_finite() {
            i.position.max(0.0)
        } else {
            0.0
        };
        if i.playing && !i.live {
            p += self.at.elapsed().as_secs_f64() * rate_of(i);
        }
        if let Some(d) = i.duration.filter(|d| d.is_finite() && *d > 0.0) {
            p = p.min(d);
        }
        p
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

fn last_element(id: &str) -> &str {
    id.rsplit_once('.').map_or(id, |(_, last)| last)
}

/// The bus name's own element: the app id's last element, else the executable's name, with
/// everything outside `[A-Za-z0-9_]` turned into `_`, never empty and never starting with a
/// digit.
fn player_name() -> String {
    let raw = env("DAY_APP_ID")
        .map(|id| last_element(&id).to_owned())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        })
        .unwrap_or_default();
    sanitize_element(&raw)
}

fn sanitize_element(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        return "day".into();
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// The playback rate a client sees: the published one when it is a rate, else real time.
fn rate_of(i: &NowPlayingInfo) -> f64 {
    if i.rate.is_finite() && i.rate > 0.0 {
        i.rate
    } else {
        1.0
    }
}

/// Seconds to the specification's microseconds.
fn micros(secs: f64) -> i64 {
    if secs.is_finite() {
        (secs * 1_000_000.0).round() as i64
    } else {
        0
    }
}

fn playback_status(info: Option<&NowPlayingInfo>) -> &'static str {
    match info {
        None => "Stopped",
        Some(i) if i.playing => "Playing",
        Some(_) => "Paused",
    }
}

/// `mpris:trackid`: an object path that names the track, stable while the same title, artist
/// and album are on, so clients can tell a new track from a changed position.
fn track_id(i: &NowPlayingInfo) -> String {
    let bytes = i
        .title
        .bytes()
        .chain([0])
        .chain(i.artist.bytes())
        .chain([0])
        .chain(i.album.bytes());
    let hash = bytes.fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("/dev/daybrite/media/track/t{hash:016x}")
}

/// A `file://` URL for a local path, percent-encoding what is not unreserved. A relative path
/// is taken from the working directory.
fn file_url(path: &str) -> String {
    let absolute = if path.starts_with('/') {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map(|d| d.join(path).to_string_lossy().into_owned())
            .unwrap_or_else(|_| format!("/{path}"))
    };
    let mut url = String::from("file://");
    for b in absolute.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                url.push(char::from(b));
            }
            _ => url.push_str(&format!("%{b:02X}")),
        }
    }
    url
}

/// `mpris:artUrl`: the local artwork as a `file://` URL, else the artwork URL as given.
fn art_url(i: &NowPlayingInfo) -> Option<String> {
    if let Some(p) = i.artwork_path.as_deref().filter(|p| !p.is_empty()) {
        return Some(if p.contains("://") {
            p.to_owned()
        } else {
            file_url(p)
        });
    }
    i.artwork_url.clone().filter(|u| !u.is_empty())
}

/// The `Metadata` dictionary; empty when nothing is on.
fn metadata(info: Option<&NowPlayingInfo>) -> Value {
    let Some(i) = info else {
        return Value::props(Vec::<(&str, Value)>::new());
    };
    let mut m = vec![("mpris:trackid", Value::path(track_id(i)))];
    if let Some(d) = i.duration.filter(|d| d.is_finite() && *d > 0.0) {
        m.push(("mpris:length", Value::I64(micros(d))));
    }
    if !i.title.is_empty() {
        m.push(("xesam:title", Value::str(&i.title)));
    }
    if !i.artist.is_empty() {
        m.push(("xesam:artist", Value::strings(&[&i.artist])));
    }
    if !i.album.is_empty() {
        m.push(("xesam:album", Value::str(&i.album)));
    }
    if let Some(url) = art_url(i) {
        m.push(("mpris:artUrl", Value::str(url)));
    }
    Value::props(m)
}

fn root_property(state: &State, name: &str) -> Option<Value> {
    Some(match name {
        "CanQuit" | "CanRaise" | "HasTrackList" => Value::Bool(false),
        "Identity" => Value::str(&state.identity),
        "DesktopEntry" => Value::str(state.desktop_entry.as_deref()?),
        "SupportedUriSchemes" | "SupportedMimeTypes" => Value::strings::<&str>(&[]),
        _ => return None,
    })
}

fn player_property(state: &State, name: &str) -> Option<Value> {
    let info = state.info.as_ref();
    Some(match name {
        "PlaybackStatus" => Value::str(playback_status(info)),
        "Rate" => Value::Double(info.map_or(1.0, rate_of)),
        "Metadata" => metadata(info),
        // The app owns its volume; a client's `Set` is accepted and ignored (see `properties`).
        "Volume" => Value::Double(1.0),
        "Position" => Value::I64(micros(state.position())),
        "MinimumRate" => Value::Double(0.25),
        "MaximumRate" => Value::Double(4.0),
        "CanGoNext" => Value::Bool(info.is_some_and(|i| i.has_next)),
        "CanGoPrevious" => Value::Bool(info.is_some_and(|i| i.has_previous)),
        "CanPlay" | "CanPause" => Value::Bool(info.is_some()),
        "CanSeek" => Value::Bool(info.is_some_and(|i| !i.live)),
        "CanControl" => Value::Bool(true),
        _ => return None,
    })
}

/// One property of either interface; an empty interface name looks in both.
fn property(state: &State, iface: &str, name: &str) -> Option<Value> {
    match iface {
        ROOT_INTERFACE => root_property(state, name),
        PLAYER_INTERFACE => player_property(state, name),
        "" => root_property(state, name).or_else(|| player_property(state, name)),
        _ => None,
    }
}

fn all_properties(state: &State, iface: &str) -> Vec<(&'static str, Value)> {
    let mut all = Vec::new();
    if matches!(iface, ROOT_INTERFACE | "") {
        all.extend(
            ROOT_PROPERTIES
                .iter()
                .filter_map(|n| root_property(state, n).map(|v| (*n, v))),
        );
    }
    if matches!(iface, PLAYER_INTERFACE | "") {
        all.extend(
            PLAYER_PROPERTIES
                .iter()
                .filter_map(|n| player_property(state, n).map(|v| (*n, v))),
        );
    }
    all
}

/// The `Player` properties whose changes are signalled: all but `Position`, which the
/// specification has clients compute from `Rate` and ask for.
fn signalled_properties(state: &State) -> Vec<(&'static str, Value)> {
    PLAYER_PROPERTIES
        .iter()
        .filter(|n| **n != "Position")
        .filter_map(|n| player_property(state, n).map(|v| (*n, v)))
        .collect()
}

fn string_arg(args: &[Value], at: usize) -> Result<&str, MethodError> {
    args.get(at)
        .and_then(Value::as_str)
        .ok_or_else(|| MethodError::invalid_args(format!("argument {at} must be a string")))
}

fn int_arg(args: &[Value], at: usize) -> Result<i64, MethodError> {
    args.get(at)
        .and_then(Value::as_i64)
        .ok_or_else(|| MethodError::invalid_args(format!("argument {at} must be an integer")))
}

/// `org.freedesktop.DBus.Properties` for both interfaces.
fn properties(state: &Mutex<State>, call: &MethodCall) -> Result<Vec<Value>, MethodError> {
    match call.member.as_str() {
        "Get" => {
            let (iface, name) = (string_arg(&call.args, 0)?, string_arg(&call.args, 1)?);
            property(&lock(state), iface, name)
                .map(|v| vec![Value::variant(v)])
                .ok_or_else(|| MethodError::unknown_property(name))
        }
        "GetAll" => {
            let iface = string_arg(&call.args, 0)?;
            Ok(vec![Value::props(all_properties(&lock(state), iface))])
        }
        "Set" => {
            let (iface, name) = (string_arg(&call.args, 0)?, string_arg(&call.args, 1)?);
            match (iface, name) {
                // Writable by the specification; the app decides both, so the write is taken
                // and changes nothing.
                (PLAYER_INTERFACE, "Volume" | "Rate") => Ok(Vec::new()),
                _ if property(&lock(state), iface, name).is_some() => Err(MethodError::new(
                    "org.freedesktop.DBus.Error.PropertyReadOnly",
                    format!("{name} is read-only"),
                )),
                _ => Err(MethodError::unknown_property(name)),
            }
        }
        _ => Err(MethodError::unknown_method(call)),
    }
}

/// A `Seek` offset in microseconds as the piece's skip command.
fn seek_text(offset_micros: i64) -> String {
    let secs = offset_micros as f64 / 1_000_000.0;
    if secs < 0.0 {
        format!("backward:{}", -secs)
    } else {
        format!("forward:{secs}")
    }
}

/// `org.mpris.MediaPlayer2.Player` methods: each becomes a `REMOTE` report for the app.
fn player_method(state: &Mutex<State>, call: &MethodCall) -> Result<Vec<Value>, MethodError> {
    let text = match call.member.as_str() {
        "Next" => "next".to_owned(),
        "Previous" => "previous".to_owned(),
        "Pause" => "pause".to_owned(),
        "PlayPause" => "toggle".to_owned(),
        "Stop" => "stop".to_owned(),
        "Play" => "play".to_owned(),
        "Seek" => seek_text(int_arg(&call.args, 0)?),
        "SetPosition" => {
            let (track, position) = (string_arg(&call.args, 0)?, int_arg(&call.args, 1)?);
            let current = lock(state).info.as_ref().map(track_id);
            // The specification: a stale track id or an impossible position is ignored.
            if current.as_deref() != Some(track) || position < 0 {
                return Ok(Vec::new());
            }
            format!("seek:{}", position as f64 / 1_000_000.0)
        }
        // Nothing to open: the app chooses what plays.
        "OpenUri" => return Ok(Vec::new()),
        _ => return Err(MethodError::unknown_method(call)),
    };
    deliver(state, text);
    Ok(Vec::new())
}

/// Hands a command to the app: a `REMOTE` report on the node that published last, on the UI
/// thread. Nothing to tell before a publish, or before the backend runs.
fn deliver(state: &Mutex<State>, text: String) {
    let node = lock(state).node;
    let (Some(node), Some(emit)) = (node, EMIT.get().copied()) else {
        return;
    };
    if !day_reactive::has_main_poster() {
        return;
    }
    day_reactive::on_main(move || emit(node, text));
}

/// Every call on the object, from the connection's reader thread.
fn handle(state: &Mutex<State>, call: &MethodCall) -> Result<Vec<Value>, MethodError> {
    match (call.interface.as_deref(), call.member.as_str()) {
        (Some(PROPERTIES_INTERFACE), _) => properties(state, call),
        (Some(INTROSPECTABLE_INTERFACE) | None, "Introspect") => {
            Ok(vec![Value::str(introspection_xml(XML, &[]))])
        }
        // Raise needs a window to bring up and Quit a way out; the piece has neither (CanQuit
        // and CanRaise say so), and a well-behaved client never calls them.
        (Some(ROOT_INTERFACE), "Raise" | "Quit") => Ok(Vec::new()),
        (Some(ROOT_INTERFACE), _) => Err(MethodError::unknown_method(call)),
        (Some(PLAYER_INTERFACE) | None, _) => player_method(state, call),
        (Some(_), _) => Err(MethodError::unknown_method(call)),
    }
}

/// Owns `org.mpris.MediaPlayer2.<name>`, or the `.instance<pid>` form the specification gives a
/// second instance of the same player.
fn own_name(conn: &Connection) -> Option<String> {
    let base = format!("{ROOT_INTERFACE}.{}", player_name());
    let instance = format!("{base}.instance{}", std::process::id());
    for name in [base, instance] {
        match conn.request_name(&name, NAME_FLAG_DO_NOT_QUEUE) {
            Ok(NAME_REPLY_PRIMARY_OWNER | NAME_REPLY_ALREADY_OWNER) => return Some(name),
            Ok(NAME_REPLY_EXISTS) => continue,
            Ok(code) => {
                log::debug!("media: MPRIS name {name} not owned (RequestName answered {code})");
                return None;
            }
            Err(e) => {
                log::debug!("media: MPRIS name {name} not owned: {e}");
                return None;
            }
        }
    }
    None
}

/// Takes one publish into the shared state and tells the bus what changed.
fn apply(conn: &Connection, state: &Mutex<State>, node: NodeId, info: Option<NowPlayingInfo>) {
    let (changed, seeked) = {
        let mut s = lock(state);
        let before = signalled_properties(&s);
        let expected = s.position();
        let previous_track = s.info.as_ref().map(track_id);
        s.node = Some(node);
        s.info = info;
        s.at = Instant::now();
        let changed: Vec<(&'static str, Value)> = signalled_properties(&s)
            .into_iter()
            .filter(|(name, value)| before.iter().any(|(n, v)| n == name && v != value))
            .collect();
        // A jump within the same track is a seek; a new track starts wherever it starts.
        let seeked = s.info.as_ref().and_then(|i| {
            let same_track = previous_track.as_deref() == Some(track_id(i).as_str());
            let jumped = (i.position - expected).abs() > SEEK_JUMP_SECS;
            (same_track && !i.live && jumped).then(|| micros(i.position))
        });
        (changed, seeked)
    };
    if !changed.is_empty()
        && let Err(e) = conn.emit_signal(
            PATH,
            PROPERTIES_INTERFACE,
            "PropertiesChanged",
            &[
                Value::str(PLAYER_INTERFACE),
                Value::props(changed),
                Value::strings::<&str>(&[]),
            ],
        )
    {
        log::debug!("media: MPRIS PropertiesChanged not sent: {e}");
    }
    if let Some(position) = seeked
        && let Err(e) = conn.emit_signal(PATH, PLAYER_INTERFACE, "Seeked", &[Value::I64(position)])
    {
        log::debug!("media: MPRIS Seeked not sent: {e}");
    }
}

/// The service thread: connect, export, own the name, then relay every publish until the
/// channel closes or the bus goes away.
fn serve(rx: mpsc::Receiver<Update>) {
    let conn = match Connection::session() {
        Ok(c) => c,
        Err(e) => {
            log::debug!("media: no MPRIS service, no session bus: {e}");
            return;
        }
    };
    let state = Arc::new(Mutex::new(State::new()));
    let handler_state = Arc::clone(&state);
    if let Err(e) = conn.export(PATH, move |call| handle(&handler_state, call)) {
        log::debug!("media: no MPRIS service, {PATH} not exported: {e}");
        return;
    }
    let Some(name) = own_name(&conn) else {
        conn.unexport(PATH);
        return;
    };
    log::debug!("media: MPRIS service up as {name}");
    while let Ok(Update { node, info }) = rx.recv() {
        if conn.is_closed() {
            log::debug!("media: MPRIS service stopped, the session bus went away");
            return;
        }
        apply(&conn, &state, node, info);
    }
    conn.unexport(PATH);
    let _ = conn.release_name(&name);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song() -> NowPlayingInfo {
        NowPlayingInfo {
            title: "Morning Light".into(),
            artist: "Aurora Fields".into(),
            album: "Dawn".into(),
            artwork_path: Some("/home/u/My Art.png".into()),
            artwork_url: Some("https://example.org/art.png".into()),
            duration: Some(180.0),
            position: 10.0,
            rate: 1.0,
            playing: true,
            live: false,
            has_next: true,
            has_previous: false,
        }
    }

    #[test]
    fn bus_name_element_is_legal() {
        assert_eq!(sanitize_element("tunes"), "tunes");
        assert_eq!(sanitize_element("9lives"), "_9lives");
        assert_eq!(sanitize_element("a-b c"), "a_b_c");
        assert_eq!(sanitize_element(""), "day");
        assert_eq!(last_element("dev.daybrite.tunes"), "tunes");
        assert_eq!(last_element("tunes"), "tunes");
    }

    #[test]
    fn local_artwork_becomes_a_file_url() {
        assert_eq!(
            file_url("/home/u/My Art.png"),
            "file:///home/u/My%20Art.png"
        );
        assert_eq!(
            art_url(&song()).as_deref(),
            Some("file:///home/u/My%20Art.png")
        );
        let mut remote = song();
        remote.artwork_path = None;
        assert_eq!(
            art_url(&remote).as_deref(),
            Some("https://example.org/art.png")
        );
    }

    #[test]
    fn metadata_carries_the_track() {
        let m = metadata(Some(&song()));
        assert_eq!(
            m.dict_get("xesam:title"),
            Some(&Value::str("Morning Light"))
        );
        assert_eq!(
            m.dict_get("xesam:artist"),
            Some(&Value::strings(&["Aurora Fields"]))
        );
        assert_eq!(m.dict_get("mpris:length"), Some(&Value::I64(180_000_000)));
        let id = m.dict_get("mpris:trackid").and_then(Value::as_str).unwrap();
        assert!(day_platform_linux::dbus::is_valid_object_path(id), "{id}");
        assert_eq!(metadata(None).as_array().map(<[Value]>::len), Some(0));
    }

    #[test]
    fn seek_offsets_split_by_sign() {
        assert_eq!(seek_text(30_000_000), "forward:30");
        assert_eq!(seek_text(-15_500_000), "backward:15.5");
        assert_eq!(
            crate::RemoteCommand::from_report(&seek_text(-15_500_000)),
            Some(crate::RemoteCommand::SkipBackward(15.5))
        );
    }

    #[test]
    fn position_runs_on_while_playing() {
        let mut s = State::new();
        s.info = Some(song());
        s.at = Instant::now() - std::time::Duration::from_secs(2);
        let p = s.position();
        assert!((11.9..12.5).contains(&p), "{p}");
        s.info.as_mut().unwrap().playing = false;
        assert_eq!(s.position(), 10.0);
        assert_eq!(playback_status(s.info.as_ref()), "Paused");
        assert_eq!(playback_status(None), "Stopped");
    }
}
