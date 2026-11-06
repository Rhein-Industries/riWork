use crate::{
    config::{self, Colors},
    maintenance, policy,
};
use nsh_core::JsonValue;
use nsh_platform_macos::{
    ComponentContext, Components, NativeComponent, objc2::rc::Retained, objc2_app_kit::NSView,
};
use riwork_engine::{
    remote_hosts::{self, RemoteCli},
    session_input::Input,
    sessions::{SessionManager, quote_arg},
    terminal_drop,
    terminal_links::{Bases, Link, PaneView},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::{CStr, CString, c_char, c_void},
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

unsafe extern "C" {
    fn rw_terminal_new(
        context: *mut c_void,
        event: extern "C" fn(*mut c_void, *const c_char, *const c_char),
        detach: extern "C" fn(*mut c_void, *mut c_void, *const c_char),
        release: extern "C" fn(*mut c_void),
    ) -> *mut NSView;
    fn rw_terminal_host_new() -> *mut NSView;
    fn rw_terminal_host_show(host: *mut c_void, view: *mut c_void);
    fn rw_terminal_pool(view: *mut c_void) -> bool;
    fn rw_terminal_live(view: *mut c_void) -> bool;
    fn rw_terminal_swap_context(a: *mut c_void, b: *mut c_void);
    fn rw_terminal_attach(
        view: *mut c_void,
        command: *const c_char,
        cwd: *const c_char,
        config: *const c_char,
    ) -> bool;
    fn rw_terminal_set_state(view: *mut c_void, visible: bool, frozen: bool, focus: bool);
    fn rw_terminal_update_config(view: *mut c_void, path: *const c_char) -> bool;
    fn rw_terminal_read_config_colors(path: *const c_char, colors: *mut u32) -> bool;
    fn rw_terminal_command(view: *mut c_void, name: *const c_char);
    fn rw_terminal_find(view: *mut c_void, query: *const c_char);
    fn rw_terminal_shutdown(view: *mut c_void);
    fn rw_terminal_abandon(view: *mut c_void);
    fn rw_terminal_detach_result(view: *mut c_void, confirmed: bool);
    fn rw_terminal_set_link(view: *mut c_void, json: *const c_char);
    fn rw_terminal_paste_text(view: *mut c_void, text: *const c_char);
    fn rw_terminal_pending_count() -> usize;
    #[cfg(feature = "terminal-fixture")]
    fn rw_terminal_monotonic_ms() -> f64;
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Target {
    Local {
        #[serde(rename = "shellId")]
        shell_id: String,
    },
    Remote {
        #[serde(rename = "desktopId")]
        desktop_id: String,
        #[serde(rename = "shellId")]
        shell_id: String,
    },
}
impl Target {
    fn local(&self) -> Option<&str> {
        if let Self::Local { shell_id } = self {
            Some(shell_id)
        } else {
            None
        }
    }
    fn valid(&self) -> bool {
        match self {
            Self::Local { shell_id } => uuid::Uuid::parse_str(shell_id).is_ok(),
            Self::Remote {
                desktop_id,
                shell_id,
            } => remote_id(desktop_id) && remote_id(shell_id),
        }
    }
    /// What `active` holds while attached: a local shell id (a UUID), or
    /// `remote:{desktop}:{shell}`, the key a remote tab saves under.
    fn key(&self) -> String {
        match self {
            Self::Local { shell_id } => shell_id.clone(),
            Self::Remote {
                desktop_id,
                shell_id,
            } => format!("{REMOTE}{desktop_id}:{shell_id}"),
        }
    }
}
const REMOTE: &str = "remote:";
fn remote_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && !id
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == ':')
}
fn is_remote(active: &str) -> bool {
    active.starts_with(REMOTE)
}
struct Model {
    target: Option<Target>,
    visible: bool,
    pinned: bool,
    frozen: bool,
    focus: bool,
    colors: Colors,
    active: Option<String>,
    attaching: bool,
    closing: bool,
    ended: bool,
    // Child exit is not proof of tmux session death. Keep its last surface
    // until an authoritative read says dead or a deliberate retry replaces it.
    child_exited: bool,
    exit_check_pending: bool,
    child_failures: u32,
    retry_at: Option<Instant>,
    failed: bool,
    recreate: bool,
    hidden: Instant,
    // What the surface was last configured with, which can lag `colors` and
    // `CONFIG_REVISION` while an update is on its way. A kept surface is
    // compared with the colors of the component that takes it.
    surface_colors: Option<Colors>,
    surface_revision: u64,
    // The app is switching this window's project, so an unmount now is the
    // project leaving, not its tab, pane or window closing (`pool`).
    keep: bool,
}
impl Default for Model {
    fn default() -> Self {
        Self {
            target: None,
            visible: false,
            pinned: false,
            frozen: false,
            focus: false,
            colors: Colors::default(),
            active: None,
            attaching: false,
            closing: false,
            ended: false,
            child_exited: false,
            exit_check_pending: false,
            child_failures: 0,
            retry_at: None,
            failed: false,
            recreate: false,
            hidden: Instant::now(),
            surface_colors: None,
            surface_revision: 0,
            keep: false,
        }
    }
}
struct State {
    id: u64,
    home: PathBuf,
    context: ComponentContext,
    // The terminal view whose context is this state's. It changes when the
    // component takes a kept terminal (`adopt`).
    view: AtomicUsize,
    // The component's own view, which NSH lays out and which hosts `view`.
    // Not retained: it is used only while the component is alive.
    host: AtomicUsize,
    alive: AtomicBool,
    generation: AtomicU64,
    config_epoch: AtomicU64,
    // A prop change's reconcile is queued on the main loop (`reconcile_soon`).
    reconcile_queued: AtomicBool,
    link_busy: AtomicBool,
    link_cache: Mutex<Option<(u64, u64, Value)>>,
    link_pending: Mutex<Option<Value>>,
    // When teardown first hung up a remote bridge; it is killed if it outlives 2 s.
    hangup: Mutex<Option<Instant>>,
    images: Mutex<crate::images::Images>,
    // The images pasted into kept terminals this component took: their shells
    // may still read them, so they last as long as this tab.
    adopted_images: Mutex<Vec<crate::images::Images>>,
    model: Mutex<Model>,
}
static NEXT: AtomicU64 = AtomicU64::new(1);
static CONFIG_REVISION: AtomicU64 = AtomicU64::new(1);
// Native reparenting can change a window's warm-terminal set without a prop
// change. AppKit reports that edge; the worker only reads this revision.
static PLACEMENT_REVISION: AtomicU64 = AtomicU64::new(0);
struct ThemeProjection {
    home: PathBuf,
    publish: Arc<dyn Fn(Result<riwork_core::theme::TerminalTheme, String>) + Send + Sync>,
}
static THEME_PROJECTION: OnceLock<ThemeProjection> = OnceLock::new();

/// Called on AppKit's main thread after startup workers finish, before AppViews.
/// Only parsing touches Ghostty; subscribers receive a plain RGB snapshot and
/// must never synchronously dispatch back here while a startup worker is joined.
pub fn initialize_theme_projection(
    home: PathBuf,
    publish: Arc<dyn Fn(Result<riwork_core::theme::TerminalTheme, String>) + Send + Sync>,
) {
    if THEME_PROJECTION.set(ThemeProjection { home, publish }).is_ok() {
        refresh_theme_projection();
    }
}

fn refresh_theme_projection() {
    let Some(projection) = THEME_PROJECTION.get() else { return; };
    let result = (|| {
        let revision = CONFIG_REVISION.load(Ordering::Acquire);
        let path = config::write(
            &projection.home,
            &format!("appearance-{}-{}", std::process::id(), uuid::Uuid::new_v4()),
            &config::roots(), &Colors::Ghostty, revision,
        )?;
        let config = ConfigFile { cstring: cstr(path.to_string_lossy())?, path, revision };
        let mut colors = [0_u32; 18];
        if !unsafe { rw_terminal_read_config_colors(config.cstring.as_ptr(), colors.as_mut_ptr()) } {
            return Err("Ghostty could not resolve its configuration colors. Check the Ghostty configuration for errors.".into());
        }
        Ok(riwork_core::theme::TerminalTheme {
            background: colors[0], foreground: colors[1],
            palette: std::array::from_fn(|index| colors[index + 2]),
        })
    })();
    (projection.publish)(result);
}
static STATES: OnceLock<Mutex<Vec<Weak<State>>>> = OnceLock::new();
static CLAIMS: OnceLock<Mutex<BTreeMap<(PathBuf, String), u64>>> = OnceLock::new();
fn claims() -> &'static Mutex<BTreeMap<(PathBuf, String), u64>> {
    CLAIMS.get_or_init(Default::default)
}
fn states() -> &'static Mutex<Vec<Weak<State>>> {
    STATES.get_or_init(Default::default)
}
fn maintenance_wake() -> &'static maintenance::Wake {
    static WAKE: OnceLock<maintenance::Wake> = OnceLock::new();
    WAKE.get_or_init(Default::default)
}
fn ptr(state: &State) -> *mut c_void {
    state.view.load(Ordering::Acquire) as *mut c_void
}
fn cstr(value: impl AsRef<str>) -> Result<CString, String> {
    CString::new(value.as_ref()).map_err(|_| "invalid: embedded NUL".into())
}
fn emit(state: &State, name: &str, data: Value) {
    if state.alive.load(Ordering::Acquire) {
        if name == "linkOpen" {
            trace(state, name, data.clone());
        }
        state.context.emit(name, data);
    }
}
fn status(state: &State, value: &str, message: Option<String>, attempt: Option<u32>) {
    emit(
        state,
        "state",
        json!({"state":value,"message":message,"attempts":attempt}),
    );
}
fn release_claim(state: &State) {
    claims()
        .lock()
        .unwrap()
        .retain(|_, owner| *owner != state.id);
}

/// The surfaces still being disposed of. The host asks only while it drains,
/// on quit or a development reload, when no component will take a kept
/// terminal any more: the pool lets all of them go, and they count until freed.
pub fn pending_count() -> usize {
    release_pooled(true);
    unsafe { rw_terminal_pending_count() }
}
/// Called once on the shared registry, before the first AppView is constructed.
pub fn register(components: &Components, home: PathBuf) {
    start_maintenance();
    let image_home = home.clone();
    thread::spawn(move || crate::images::cleanup_stale(&image_home));
    components.register(
        "riwork.terminal",
        Rc::new(move |context| {
            let state = Arc::new(State {
                id: NEXT.fetch_add(1, Ordering::Relaxed),
                home: home.clone(),
                images: Mutex::new(crate::images::Images::new(&home)),
                context,
                view: AtomicUsize::new(0),
                host: AtomicUsize::new(0),
                alive: AtomicBool::new(true),
                generation: AtomicU64::new(0),
                config_epoch: AtomicU64::new(0),
                reconcile_queued: AtomicBool::new(false),
                link_busy: AtomicBool::new(false),
                link_cache: Mutex::new(None),
                link_pending: Mutex::new(None),
                hangup: Mutex::new(None),
                adopted_images: Mutex::new(Vec::new()),
                model: Mutex::new(Model::default()),
            });
            let raw_context = Box::into_raw(Box::new(state.clone())).cast();
            let host = unsafe { Retained::from_raw(rw_terminal_host_new()) }
                .expect("create terminal host");
            let raw = unsafe { rw_terminal_new(raw_context, on_event, on_detach, on_release) };
            let view = unsafe { Retained::from_raw(raw) }.expect("create terminal NSView");
            // The host holds the terminal view from here on.
            unsafe { rw_terminal_host_show(Retained::as_ptr(&host) as *mut c_void, raw.cast()) };
            drop(view);
            state.view.store(raw as usize, Ordering::Release);
            state
                .host
                .store(Retained::as_ptr(&host) as usize, Ordering::Release);
            states().lock().unwrap().push(Arc::downgrade(&state));
            maintenance_wake().notify();
            Box::new(Terminal { host, state })
        }),
    );
}
struct Terminal {
    host: Retained<NSView>,
    state: Arc<State>,
}
impl NativeComponent for Terminal {
    fn view(&self) -> Retained<NSView> {
        self.host.clone()
    }
    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        let value = value.cloned().unwrap_or(Value::Null);
        let mut update = false;
        let mut check_exit = false;
        {
            let mut m = self.state.model.lock().unwrap();
            match name {
                "target" => match serde_json::from_value::<Target>(value) {
                    Ok(target) if target.valid() => {
                        if m.target.as_ref() != Some(&target) {
                            if m.active.is_none() {
                                release_claim(&self.state);
                            }
                            m.target = Some(target);
                            m.failed = false;
                            m.ended = false;
                            m.child_exited = false;
                            m.exit_check_pending = false;
                            m.child_failures = 0;
                            m.retry_at = None;
                            m.recreate = m.active.is_some();
                            self.state.generation.fetch_add(1, Ordering::AcqRel);
                            m.attaching = false;
                        }
                    }
                    _ => {
                        drop(m);
                        status(
                            &self.state,
                            "failed",
                            Some("invalid: terminal target".into()),
                            None,
                        );
                        return;
                    }
                },
                "visible" => {
                    let visible = value.as_bool().unwrap_or(false);
                    if m.visible != visible {
                        m.visible = visible;
                        m.hidden = Instant::now();
                        if visible {
                            m.failed = false;
                            m.child_failures = 0;
                            m.retry_at = None;
                            check_exit = m.child_exited && !m.ended;
                        }
                    }
                }
                "pinned" => m.pinned = value.as_bool().unwrap_or(false),
                "frozen" => m.frozen = value.as_bool().unwrap_or(false),
                // Read only when the component unmounts: nothing to apply now.
                "keepOnUnmount" => {
                    m.keep = value.as_bool().unwrap_or(false);
                    return;
                }
                "focusOnAttach" => m.focus = value.as_bool().unwrap_or(false),
                "colors" => match serde_json::from_value::<Colors>(value)
                    .and_then(|c| c.validate().map(|_| c).map_err(serde::de::Error::custom))
                {
                    Ok(colors) => {
                        if colors != m.colors {
                            self.state.config_epoch.fetch_add(1, Ordering::AcqRel);
                            m.recreate = matches!(colors, Colors::Ghostty)
                                && m.active.is_some()
                                && !m.child_exited;
                            m.colors = colors;
                            update = true;
                        }
                    }
                    Err(e) => {
                        drop(m);
                        status(&self.state, "failed", Some(e.to_string()), None);
                        return;
                    }
                },
                _ => return,
            }
        }
        sync_state(&self.state);
        if check_exit {
            check_child_exit(self.state.clone());
        }
        if update {
            update_config(self.state.clone());
        }
        reconcile_soon(self.state.clone());
    }
    fn command(&mut self, name: &str, args: &JsonValue) {
        match name {
            "focus" => {
                let mut m = self.state.model.lock().unwrap();
                m.failed = false;
                m.child_failures = 0;
                // An attached view receives the command immediately. Only a
                // pending/recreated surface needs a deferred focus request.
                m.focus = m.active.is_none() || m.closing || m.recreate;
                let check_exit = m.child_exited && !m.ended;
                if check_exit {
                    m.retry_at = None;
                }
                drop(m);
                sync_state(&self.state);
                if check_exit {
                    check_child_exit(self.state.clone());
                }
                reconcile(self.state.clone());
            }
            "reattach" => {
                let mut m = self.state.model.lock().unwrap();
                self.state.generation.fetch_add(1, Ordering::AcqRel);
                m.attaching = false;
                if m.active.is_none() {
                    release_claim(&self.state);
                }
                m.failed = false;
                m.ended = false;
                m.exit_check_pending = false;
                m.child_failures = 0;
                m.retry_at = None;
                // A remote bridge that exited is replaced by a new one; a local
                // client's exit is first checked against its tmux session.
                let remote = m.active.as_deref().is_some_and(is_remote);
                let check_exit = m.child_exited && m.active.is_some() && !remote;
                m.recreate = m.active.is_some() && !check_exit;
                drop(m);
                if check_exit {
                    check_child_exit(self.state.clone());
                }
                reconcile(self.state.clone());
                return;
            }
            "find" => {
                if let Some(query) = args.get("query").and_then(Value::as_str) {
                    if let Ok(query) = cstr(query) {
                        unsafe { rw_terminal_find(ptr(&self.state), query.as_ptr()) };
                    }
                }
                return;
            }
            "blur" | "copy" | "selectAll" | "paste" | "findNext" | "findPrevious" | "findClose" => {
            }
            _ => return, // Reserved commands are deliberately not accepted.
        }
        if let Ok(name) = cstr(name) {
            unsafe { rw_terminal_command(ptr(&self.state), name.as_ptr()) };
        }
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        self.state.alive.store(false, Ordering::Release);
        self.state.generation.fetch_add(1, Ordering::AcqRel);
        if pool(&self.state) {
            return;
        }
        unsafe { rw_terminal_abandon(ptr(&self.state)) };
        if self.state.model.lock().unwrap().active.is_none() {
            release_claim(&self.state);
        }
    }
}
fn sync_state(state: &State) {
    let (v, f, focus) = {
        let m = state.model.lock().unwrap();
        (m.visible, m.frozen, m.focus)
    };
    unsafe { rw_terminal_set_state(ptr(state), v, f, focus) };
}

/// A local terminal kept after its component unmounted because the window
/// switched to another project, with its surface and tmux client, so that the
/// next component for its shell shows it at once instead of attaching anew. Its
/// state is dead (not alive, its generation moved on), so work it had in
/// flight lands nowhere. It keeps the shell's claim until a component takes it
/// (`adopt`) or the pool lets it go through the ordinary abandon path.
struct Pooled {
    state: Arc<State>,
    shell: String,
    // The terminal view, retained here while no host holds it.
    view: usize,
    // When it was last on screen.
    shown: Instant,
    colors: Option<Colors>,
    revision: u64,
    // Its client exited while it waited.
    ended: bool,
}
// Used only on the main thread; a lock only because it is a static.
static POOL: Mutex<Vec<Pooled>> = Mutex::new(Vec::new());
// When the pool next needs a maintenance pass, on `clock_ms`; never: u64::MAX.
static POOL_DUE: AtomicU64 = AtomicU64::new(u64::MAX);
fn clock_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Keeps a dropped component's terminal if the app was switching its window's
/// project (`keepOnUnmount`) and it shows its local shell and is healthy:
/// attached, not about to be replaced, its client and session alive as far as
/// anyone knows, and on screen within `WARM_EXPIRY`. A closed tab, pane or
/// window, anything else and anything uncertain is abandoned as before, so
/// its client detaches at once (Release = detach).
fn pool(state: &Arc<State>) -> bool {
    let (shell, shown, colors, revision) = {
        let m = state.model.lock().unwrap();
        if !m.keep {
            return false;
        }
        let Some(shell) = m.target.as_ref().and_then(Target::local) else {
            return false;
        };
        let healthy = m.active.as_deref() == Some(shell)
            && !(m.attaching
                || m.closing
                || m.recreate
                || m.ended
                || m.child_exited
                || m.exit_check_pending
                || m.failed
                || m.pinned)
            && m.retry_at.is_none();
        let shown = if m.visible { Instant::now() } else { m.hidden };
        if !healthy || shown.elapsed() >= policy::WARM_EXPIRY {
            return false;
        }
        let colors = m.surface_colors.clone();
        (shell.to_owned(), shown, colors, m.surface_revision)
    };
    let key = (state.home.clone(), shell.clone());
    if claims().lock().unwrap().get(&key) != Some(&state.id) {
        return false;
    }
    let raw = ptr(state);
    let Some(view) = (unsafe { Retained::retain(raw.cast::<NSView>()) }) else {
        return false;
    };
    if !unsafe { rw_terminal_pool(raw) } {
        return false;
    }
    POOL.lock().unwrap().push(Pooled {
        state: state.clone(),
        shell,
        view: Retained::into_raw(view) as usize,
        shown,
        colors,
        revision,
        ended: false,
    });
    schedule_pool();
    true
}
fn pool_tabs(pool: &[Pooled]) -> Vec<policy::PooledTab> {
    let revision = CONFIG_REVISION.load(Ordering::Acquire);
    pool.iter()
        .map(|kept| policy::PooledTab {
            id: kept.state.id,
            shown_ago: kept.shown.elapsed(),
            stale: kept.ended || kept.revision != revision,
        })
        .collect()
}
/// Tells the maintenance thread when the pool next needs a pass.
fn schedule_pool() {
    let plan = policy::plan_pool(&pool_tabs(&POOL.lock().unwrap()));
    let due = if !plan.release.is_empty() {
        0
    } else {
        plan.recheck
            .map_or(u64::MAX, |wait| clock_ms() + wait.as_millis() as u64)
    };
    if due < POOL_DUE.swap(due, Ordering::AcqRel) {
        maintenance_wake().notify();
    }
}
/// Lets kept terminals go: those the pool's policy releases now, or every one.
fn release_pooled(all: bool) {
    let released = {
        let mut pool = POOL.lock().unwrap();
        let chosen = (!all).then(|| policy::plan_pool(&pool_tabs(&pool)).release);
        let (released, kept): (Vec<_>, Vec<_>) =
            std::mem::take(&mut *pool).into_iter().partition(|kept| {
                chosen
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&kept.state.id))
            });
        *pool = kept;
        released
    };
    for kept in released {
        release(kept);
    }
    schedule_pool();
}
/// The ordinary abandon path. Disposal retains the view until it is freed,
/// and the old state's `disposed` then releases the shell's claim.
fn release(kept: Pooled) {
    trace(
        &kept.state,
        "pool-released",
        json!({"shellId":kept.shell,"ended":kept.ended}),
    );
    unsafe { rw_terminal_abandon(kept.view as *mut c_void) };
    drop(unsafe { Retained::from_raw(kept.view as *mut NSView) });
}
/// A kept terminal's client exited, so it has nothing left to show: the next
/// pass lets it go. False when `state` is not a kept terminal's.
fn pool_ended(state: &State) -> bool {
    let found = POOL
        .lock()
        .unwrap()
        .iter_mut()
        .find(|kept| kept.state.id == state.id)
        .map(|kept| kept.ended = true)
        .is_some();
    if found {
        schedule_pool();
    }
    found
}
/// The kept terminal of a shell, taken out of the pool.
fn take_pooled(home: &Path, shell: &str) -> Option<Pooled> {
    let mut pool = POOL.lock().unwrap();
    let index = pool
        .iter()
        .position(|kept| kept.state.home == home && kept.shell == shell)?;
    Some(pool.remove(index))
}
/// `state`'s component wants the shell a kept terminal shows. It takes the
/// terminal if that is still live. One whose client has exited shows nothing
/// of the shell, so it is let go and the claim passes on at once: the new
/// attach does not wait for it to be freed.
fn take_over(state: Arc<State>, kept: Pooled) {
    schedule_pool();
    if !kept.ended && unsafe { rw_terminal_live(kept.view as *mut c_void) } {
        adopt(state, kept);
        return;
    }
    claims()
        .lock()
        .unwrap()
        .insert((state.home.clone(), kept.shell.clone()), state.id);
    release(kept);
    reconcile(state);
}
/// Shows a kept terminal in `state`'s component in place of its unused new
/// view, as if it had just attached. The kept view takes this component's
/// context, so it emits here from now on, and the shell's claim moves here;
/// the old state is left with the new view and nothing else.
fn adopt(state: Arc<State>, kept: Pooled) {
    let fresh = ptr(&state);
    let view = kept.view as *mut c_void;
    let old = &kept.state;
    claims()
        .lock()
        .unwrap()
        .insert((state.home.clone(), kept.shell.clone()), state.id);
    unsafe { rw_terminal_swap_context(fresh, view) };
    state.view.store(view as usize, Ordering::Release);
    old.view.store(fresh as usize, Ordering::Release);
    old.model.lock().unwrap().active = None;
    // Its shell may still read images pasted into it.
    let images = std::mem::replace(
        &mut *old.images.lock().unwrap(),
        crate::images::Images::new(&old.home),
    );
    let mut adopted = state.adopted_images.lock().unwrap();
    adopted.push(images);
    // And those of terminals it took in turn, so that a second switch back
    // does not delete the first one's images while the same shell runs.
    adopted.append(&mut old.adopted_images.lock().unwrap());
    drop(adopted);
    let (recreate, recolor) = {
        let mut m = state.model.lock().unwrap();
        m.active = Some(kept.shell.clone());
        m.ended = false;
        m.child_exited = false;
        m.exit_check_pending = false;
        m.retry_at = None;
        m.failed = false;
        // As when a shown terminal's colors change: an app palette updates
        // the surface, while Ghostty's own colors, or a Ghostty configuration
        // that changed meanwhile, replace it.
        let recolor = kept.colors.as_ref() != Some(&m.colors);
        m.recreate = kept.revision != CONFIG_REVISION.load(Ordering::Acquire)
            || recolor && matches!(m.colors, Colors::Ghostty);
        m.surface_colors = kept.colors.clone();
        m.surface_revision = kept.revision;
        (m.recreate, recolor)
    };
    // The new view never had a surface: this only retires it.
    unsafe { rw_terminal_abandon(fresh) };
    sync_state(&state);
    trace(
        &state,
        "adopted",
        json!({"from":old.id,"shellId":kept.shell}),
    );
    status(&state, "attached", None, None);
    // The host releases the new view, and with it the old state.
    unsafe { rw_terminal_host_show(state.host.load(Ordering::Acquire) as *mut c_void, view) };
    drop(unsafe { Retained::from_raw(view.cast::<NSView>()) });
    if recreate {
        reconcile(state);
    } else if recolor {
        update_config(state);
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Pasted {
    Text {
        text: String,
    },
    Files {
        paths: Vec<PathBuf>,
    },
    Image {
        #[serde(rename = "pngBase64")]
        png_base64: String,
    },
}
// All input from pasteboards, drops and component commands enters the same
// per-shell engine lock. In particular, text is never replayed after refusal.
fn request_input(state: Arc<State>, payload: Value) {
    let input = match serde_json::from_value::<Pasted>(payload) {
        Ok(Pasted::Files { paths }) if paths.is_empty() => {
            emit(
                &state,
                "inputRefused",
                json!({"reason":"No files were supplied"}),
            );
            return;
        }
        Ok(input) => input,
        Err(_) => {
            emit(
                &state,
                "inputRefused",
                json!({"reason":"Invalid terminal paste payload"}),
            );
            return;
        }
    };
    if let Pasted::Files { paths } = &input {
        if let Err(reason) = terminal_drop::validate_paths(paths) {
            emit(&state, "inputRefused", json!({"reason":reason}));
            return;
        }
    }
    let remote = {
        let m = state.model.lock().unwrap();
        m.active.as_deref().is_some_and(is_remote)
            && state.alive.load(Ordering::Acquire)
            && m.visible
            && !m.frozen
            && !m.closing
            && !m.ended
    };
    if remote {
        // As in the GPUI desktop: text goes to Ghostty's paste, which the bridge
        // forwards to the host; dropped or copied files and pictures have no
        // local shell to be given to, so nothing is pasted.
        if let Pasted::Text { text } = input {
            if let Ok(text) = cstr(text) {
                unsafe { rw_terminal_paste_text(ptr(&state), text.as_ptr()) };
            }
        }
        return;
    }
    let shell = {
        let m = state.model.lock().unwrap();
        if !state.alive.load(Ordering::Acquire) || !m.visible || m.frozen || m.closing || m.ended {
            None
        } else {
            m.active
                .clone()
                .filter(|shell| m.target.as_ref().and_then(Target::local) == Some(shell.as_str()))
        }
    };
    let Some(shell) = shell else {
        emit(
            &state,
            "inputRefused",
            json!({"reason":"Terminal is not available for input"}),
        );
        return;
    };
    if matches!(&input, Pasted::Files { .. }) {
        if let Ok(focus) = cstr("focus") {
            unsafe { rw_terminal_command(ptr(&state), focus.as_ptr()) };
        }
    }
    let generation = state.generation.load(Ordering::Acquire);
    thread::spawn(move || {
        let mut staged = None;
        let mut attachments = None;
        let result = (|| {
            let input = match input {
                Pasted::Image { png_base64 } => {
                    let path = state.images.lock().unwrap().stage(&png_base64)?;
                    staged = Some(path.clone());
                    Pasted::Files { paths: vec![path] }
                }
                input => input,
            };
            if let Pasted::Files { paths } = &input {
                terminal_drop::validate_paths(paths)?;
                attachments =
                    Some(json!({"paths":paths,"images":staged.iter().collect::<Vec<_>>() }));
            }
            let manager = SessionManager::at(state.home.clone())?;
            let mut stale = false;
            manager.paste(&shell, |harness, command, tty| {
                let m = state.model.lock().unwrap();
                if !state.alive.load(Ordering::Acquire)
                    || state.generation.load(Ordering::Acquire) != generation
                    || m.active.as_deref() != Some(&shell)
                    || m.closing
                    || m.frozen
                    || !m.visible
                {
                    stale = true;
                    return Vec::new();
                }
                drop(m);
                match input {
                    Pasted::Text { text } => vec![Input::Paste(text)],
                    Pasted::Files { paths } => {
                        let program = terminal_drop::program(harness, command, || {
                            terminal_drop::foreground_names(tty)
                        });
                        terminal_drop::path_input(program, &paths)
                    }
                    Pasted::Image { .. } => unreachable!(),
                }
            })?;
            if stale {
                return Err("Terminal changed before input could be delivered".to_owned());
            }
            Ok::<_, String>(())
        })();
        if result.is_err() {
            if let Some(path) = staged {
                let _ = std::fs::remove_file(path);
            }
        }
        dispatch2::DispatchQueue::main().exec_async(move || {
            trace(
                &state,
                "input-result",
                json!({"ok":result.is_ok(),"shellId":shell}),
            );
            if let Some(mut attachment) = attachments {
                attachment["accepted"] = json!(result.is_ok());
                if let Err(reason) = &result {
                    attachment["reason"] = json!(reason);
                }
                if result.is_ok() && state.generation.load(Ordering::Acquire) == generation {
                    if let Ok(focus) = cstr("focus") {
                        unsafe { rw_terminal_command(ptr(&state), focus.as_ptr()) };
                    }
                }
                emit(&state, "drop", attachment);
            }
            if let Err(reason) = result {
                emit(&state, "inputRefused", json!({"reason":reason}));
            }
        });
    });
}
/// NSH sets a terminal's props one at a time, in the order the component
/// declares them, and `visible` comes before `colors`. Reconciling after each
/// one would start a new terminal's attach with the colors known so far, and
/// the colors that follow would discard it and start a second. A batch is set
/// within one main-loop turn, so reconciling once at the end of the turn
/// prepares once, with the final props.
fn reconcile_soon(state: Arc<State>) {
    if state.reconcile_queued.swap(true, Ordering::AcqRel) {
        return;
    }
    dispatch2::DispatchQueue::main().exec_async(move || {
        state.reconcile_queued.store(false, Ordering::Release);
        reconcile(state);
    });
}
fn reconcile(state: Arc<State>) {
    if !state.alive.load(Ordering::Acquire) {
        return;
    }
    let (target, shell, colors, generation, config_epoch) = {
        let mut m = state.model.lock().unwrap();
        if m.closing {
            return;
        }
        if m.active.is_some() {
            if m.recreate {
                m.closing = true;
                drop(m);
                unsafe { rw_terminal_shutdown(ptr(&state)) };
            }
            return;
        }
        if m.attaching || !m.visible || m.failed || m.ended {
            return;
        }
        let Some(target) = m.target.clone() else {
            return;
        };
        let shell = target.key();
        // A local tmux session is shown by one surface at a time. Remote tabs
        // each run their own bridge, as every GPUI window did.
        if target.local().is_some() {
            // A kept terminal of this shell holds its claim: it is taken over,
            // not attached anew.
            if let Some(kept) = take_pooled(&state.home, &shell) {
                drop(m);
                take_over(state, kept);
                return;
            }
            let key = (state.home.clone(), shell.clone());
            let mut owners = claims().lock().unwrap();
            if owners.get(&key).is_some_and(|id| *id != state.id) {
                drop(owners);
                drop(m);
                status(&state, "claimed", None, None);
                return;
            }
            owners.insert(key, state.id);
        }
        m.attaching = true;
        m.recreate = false;
        (
            target,
            shell,
            m.colors.clone(),
            state.generation.load(Ordering::Acquire),
            state.config_epoch.load(Ordering::Acquire),
        )
    };
    status(&state, "attaching", None, Some(1));
    thread::spawn(move || {
        let mut result = Err("unavailable: attach not attempted".to_string());
        for attempt in 1..=3 {
            if !state.alive.load(Ordering::Acquire)
                || state.generation.load(Ordering::Acquire) != generation
            {
                return;
            }
            result = match &target {
                Target::Local { .. } => prepare(&state, &shell, &colors),
                Target::Remote {
                    desktop_id,
                    shell_id,
                } => prepare_remote(&state, desktop_id, shell_id, &colors),
            };
            if result.is_ok() || result.as_ref().is_err_and(|e| e.starts_with("gone:")) {
                break;
            }
            status(
                &state,
                if attempt == 3 { "failed" } else { "attaching" },
                result.as_ref().err().cloned(),
                Some(attempt),
            );
            if attempt < 3 {
                thread::sleep(Duration::from_secs(2));
            }
        }
        dispatch2::DispatchQueue::main().exec_async(move || {
            if !state.alive.load(Ordering::Acquire)
                || state.generation.load(Ordering::Acquire) != generation
            {
                return;
            }
            state.model.lock().unwrap().attaching = false;
            if state.config_epoch.load(Ordering::Acquire) != config_epoch {
                release_claim(&state);
                reconcile(state);
                return;
            }
            if !state.model.lock().unwrap().visible {
                release_claim(&state);
                return;
            }
            match result {
                Ok((command, cwd, config)) => {
                    {
                        let mut m = state.model.lock().unwrap();
                        m.active = Some(shell);
                        m.ended = false;
                        m.child_exited = false;
                        m.exit_check_pending = false;
                        m.retry_at = None;
                        m.surface_colors = Some(colors);
                        m.surface_revision = config.revision;
                    }
                    sync_state(&state);
                    if !unsafe {
                        rw_terminal_attach(
                            ptr(&state),
                            command.as_ptr(),
                            cwd.as_ptr(),
                            config.cstring.as_ptr(),
                        )
                    } {
                        let mut m = state.model.lock().unwrap();
                        m.active = None;
                        m.failed = true;
                        drop(m);
                        release_claim(&state);
                        status(
                            &state,
                            "failed",
                            Some("libghostty could not create the terminal surface".into()),
                            Some(3),
                        );
                    }
                }
                Err(error) => {
                    let gone = error.starts_with("gone:");
                    let mut m = state.model.lock().unwrap();
                    m.failed = !gone;
                    m.ended = gone;
                    drop(m);
                    release_claim(&state);
                    status(
                        &state,
                        if gone { "gone" } else { "failed" },
                        Some(error),
                        Some(3),
                    );
                }
            }
        });
    });
}

struct ConfigFile {
    path: PathBuf,
    cstring: CString,
    // The `CONFIG_REVISION` it was written for.
    revision: u64,
}
impl ConfigFile {
    fn new(state: &State, colors: &Colors) -> Result<Self, String> {
        let revision = CONFIG_REVISION.load(Ordering::Acquire);
        let path = config::write(
            &state.home,
            &format!(
                "{}-{}-{}",
                std::process::id(),
                state.id,
                uuid::Uuid::new_v4()
            ),
            &config::roots(),
            colors,
            revision,
        )?;
        let cstring = cstr(path.to_string_lossy())?;
        Ok(Self {
            path,
            cstring,
            revision,
        })
    }
}
impl Drop for ConfigFile {
    fn drop(&mut self) {
        let path = self.path.clone();
        thread::spawn(move || {
            let _ = std::fs::remove_file(path.with_extension("override.conf"));
            let _ = std::fs::remove_file(path);
        });
    }
}

fn prepare(
    state: &State,
    shell: &str,
    colors: &Colors,
) -> Result<(CString, CString, ConfigFile), String> {
    let manager = SessionManager::at(state.home.clone())?;
    let Some((session, command)) = manager.terminal_attach(shell)? else {
        return Err("gone: the tmux session has ended".into());
    };
    let mut argv = vec!["/usr/bin/env".to_owned()];
    // Preserve the engine's TERM/TERMINFO and unset semantics when passing the
    // command to Ghostty's PTY rather than spawning it here.
    for (key, value) in command.get_envs().filter(|(_, v)| v.is_none()) {
        let _ = value;
        argv.push("-u".into());
        argv.push(key.to_string_lossy().into_owned());
    }
    for (key, value) in command.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))) {
        argv.push(format!(
            "{}={}",
            key.to_string_lossy(),
            value.to_string_lossy()
        ));
    }
    argv.push(command.get_program().to_string_lossy().into_owned());
    argv.extend(command.get_args().map(|s| s.to_string_lossy().into_owned()));
    let config = ConfigFile::new(state, colors)?;
    Ok((
        cstr(
            argv.iter()
                .map(|s| quote_arg(s))
                .collect::<Vec<_>>()
                .join(" "),
        )?,
        cstr(session.cwd.to_string_lossy())?,
        config,
    ))
}
/// The command a remote tab's terminal runs: the bridge to one shell of one
/// host, each argument shell-quoted. The bridge is the Ghostty child; the
/// host's tmux client travels through it.
fn remote_attach_command(binary: &Path, desktop_id: &str, shell_id: &str) -> String {
    std::iter::once(binary.to_string_lossy().into_owned())
        .chain(remote_hosts::attach_args(desktop_id, shell_id))
        .map(|argument| quote_arg(&argument))
        .collect::<Vec<_>>()
        .join(" ")
}
/// Located again for every attach, so installing the companion while Somtus
/// runs is enough. The bridge only needs a local directory that exists.
fn prepare_remote(
    state: &State,
    desktop_id: &str,
    shell_id: &str,
    colors: &Colors,
) -> Result<(CString, CString, ConfigFile), String> {
    let cli = RemoteCli::locate()?;
    let cwd = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_dir())
        .unwrap_or_else(|| PathBuf::from("/"));
    let config = ConfigFile::new(state, colors)?;
    Ok((
        cstr(remote_attach_command(cli.binary(), desktop_id, shell_id))?,
        cstr(cwd.to_string_lossy())?,
        config,
    ))
}
/// Teardown of a remote tab: hang up the bridge on the surface's tty (it exits
/// 129), never the shell on the host. One that outlives 2 s is killed.
fn hang_up_bridge(state: &State, tty: &str) -> Result<(), String> {
    let name = tty
        .strip_prefix("/dev/")
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 64
                && name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        })
        .ok_or("invalid terminal tty")?;
    let signal = {
        let mut first = state.hangup.lock().unwrap();
        let started = *first.get_or_insert_with(Instant::now);
        if started.elapsed() >= Duration::from_secs(2) {
            "-KILL"
        } else {
            "-HUP"
        }
    };
    let status = Command::new("/usr/bin/pkill")
        .args([signal, "-t", name])
        .status()
        .map_err(|error| format!("cannot signal the remote bridge: {error}"))?;
    // 1: nothing on that tty any more, which is what teardown waits for.
    match status.code() {
        Some(0 | 1) => Ok(()),
        _ => Err(format!("cannot signal the remote bridge: {status}")),
    }
}
fn update_config(state: Arc<State>) {
    let (colors, recreate, attached) = {
        let m = state.model.lock().unwrap();
        (m.colors.clone(), m.recreate, m.active.is_some())
    };
    if recreate {
        reconcile(state);
        return;
    }
    // Without a surface there is nothing to update: the next attach writes its
    // config with these colors, and one in flight is discarded for them
    // (`config_epoch`). A new terminal's first colors arrive this way.
    if !attached {
        return;
    }
    let generation = state.generation.load(Ordering::Acquire);
    let epoch = state.config_epoch.load(Ordering::Acquire);
    thread::spawn(move || {
        let result = ConfigFile::new(&state, &colors);
        dispatch2::DispatchQueue::main().exec_async(move || {
            if state.alive.load(Ordering::Acquire)
                && state.generation.load(Ordering::Acquire) == generation
                && state.config_epoch.load(Ordering::Acquire) == epoch
            {
                if let Ok(path) = result
                    && unsafe { rw_terminal_update_config(ptr(&state), path.cstring.as_ptr()) }
                {
                    let mut m = state.model.lock().unwrap();
                    m.surface_colors = Some(colors);
                    m.surface_revision = path.revision;
                }
            }
        });
    });
}
extern "C" fn on_release(context: *mut c_void) {
    let state = unsafe { Box::from_raw(context.cast::<Arc<State>>()) };
    trace(&state, "context-released", Value::Null);
    release_claim(&state);
}
extern "C" fn on_detach(context: *mut c_void, view: *mut c_void, tty: *const c_char) {
    let state = unsafe { &*context.cast::<Arc<State>>() }.clone();
    let tty = unsafe { CStr::from_ptr(tty) }
        .to_string_lossy()
        .into_owned();
    let shell = state.model.lock().unwrap().active.clone();
    let view = view as usize;
    thread::spawn(move || {
        let result = shell
            .ok_or("missing terminal shell".to_owned())
            .and_then(|shell| {
                if is_remote(&shell) {
                    hang_up_bridge(&state, &tty)
                } else {
                    SessionManager::at(state.home.clone())?.detach_terminal_client(&shell, &tty)
                }
            });
        if let Err(error) = &result {
            eprintln!("somtus terminal parked: {error}");
        }
        dispatch2::DispatchQueue::main().exec_async(move || {
            unsafe { rw_terminal_detach_result(view as *mut c_void, result.is_ok()) };
        });
    });
}
extern "C" fn on_event(context: *mut c_void, name: *const c_char, data: *const c_char) {
    let state = unsafe { &*context.cast::<Arc<State>>() }.clone();
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    let value: Value =
        serde_json::from_slice(unsafe { CStr::from_ptr(data) }.to_bytes()).unwrap_or(Value::Null);
    trace(&state, &name, value.clone());
    match name.as_ref() {
        "placementChanged" => { PLACEMENT_REVISION.fetch_add(1, Ordering::AcqRel); }
        "ready" => status(&state, "attached", None, None),
        "ended" => {
            if pool_ended(&state) {
                return;
            }
            let remote = state
                .model
                .lock()
                .unwrap()
                .active
                .as_deref()
                .is_some_and(is_remote);
            if remote {
                remote_bridge_exit(&state);
            } else {
                check_child_exit(state);
            }
        }
        "disposed" => {
            *state.hangup.lock().unwrap() = None;
            let mut m = state.model.lock().unwrap();
            m.active = None;
            m.closing = false;
            m.recreate = false;
            m.child_exited = false;
            m.exit_check_pending = false;
            m.retry_at = None;
            drop(m);
            release_claim(&state);
            status(&state, "detached", None, None);
            // Never create a new surface recursively from inside native free.
            dispatch2::DispatchQueue::main().exec_async(move || reconcile(state));
        }
        "focusChange" => {
            if value["focused"].as_bool() == Some(true) {
                state.model.lock().unwrap().focus = false;
            }
            emit(&state, &name, value);
        }
        "inputRefused" | "findResult" | "findOpen" => emit(&state, &name, value),
        "input" => request_input(state, value),
        "linkHit" => resolve_link(state, value),
        _ => {}
    }
}

/// A remote tab ends when its bridge exits, whatever the reason: the shell
/// ended on the host, the stream closed, or the client daemon never answered.
/// The last screen stays under the strip until Reconnect replaces the bridge.
fn remote_bridge_exit(state: &State) {
    let mut m = state.model.lock().unwrap();
    if m.closing || m.ended || m.active.is_none() {
        return;
    }
    m.child_exited = true;
    m.ended = true;
    m.failed = false;
    m.recreate = false;
    m.retry_at = None;
    drop(m);
    status(state, "ended", None, None);
}

#[derive(Debug, PartialEq, Eq)]
enum ExitDecision {
    Gone,
    Retry(u32),
    Failed(u32),
    Unknown(String),
}

fn exit_decision(liveness: Result<bool, String>, previous_failures: u32) -> ExitDecision {
    match liveness {
        Ok(false) => ExitDecision::Gone,
        Ok(true) => {
            let failures = previous_failures.saturating_add(1);
            if failures < 3 {
                ExitDecision::Retry(failures)
            } else {
                ExitDecision::Failed(failures)
            }
        }
        Err(error) => ExitDecision::Unknown(error),
    }
}

/// Runs only after Ghostty says its attach child exited, never for ordinary
/// detach/close. A stopped tmux client is different from a stopped tmux session.
fn check_child_exit(state: Arc<State>) {
    if !state.alive.load(Ordering::Acquire) {
        return;
    }
    let (shell, generation) = {
        let mut model = state.model.lock().unwrap();
        if model.closing || model.ended || model.exit_check_pending || model.retry_at.is_some() {
            return;
        }
        let Some(shell) = model.active.clone() else {
            return;
        };
        model.child_exited = true;
        model.exit_check_pending = true;
        model.recreate = false;
        (shell, state.generation.load(Ordering::Acquire))
    };
    status(
        &state,
        "attaching",
        Some("Checking the persistent terminal session".into()),
        None,
    );
    thread::spawn(move || {
        let mut liveness = Err("Session status unavailable".to_owned());
        // Query failures are unknown, never permission to destroy/recreate the
        // last screen. Retry reads off-main, then leave an explicit failed state.
        for attempt in 1..=3 {
            if !state.alive.load(Ordering::Acquire)
                || state.generation.load(Ordering::Acquire) != generation
            {
                return;
            }
            liveness = SessionManager::at(state.home.clone()).and_then(|manager| {
                manager.invalidate_sample();
                manager.get(&shell).map(|session| session.alive)
            });
            if liveness.is_ok() || attempt == 3 {
                break;
            }
            thread::sleep(Duration::from_secs(2));
        }
        dispatch2::DispatchQueue::main().exec_async(move || {
            if !state.alive.load(Ordering::Acquire)
                || state.generation.load(Ordering::Acquire) != generation
            {
                return;
            }
            let mut model = state.model.lock().unwrap();
            if model.closing || model.active.as_deref() != Some(&shell) {
                return;
            }
            model.exit_check_pending = false;
            let decision = exit_decision(liveness, model.child_failures);
            let (phase, message, attempts) = match decision {
                ExitDecision::Gone => {
                    model.ended = true;
                    model.failed = false;
                    model.retry_at = None;
                    ("gone", None, None)
                }
                ExitDecision::Retry(failures) => {
                    model.child_failures = failures;
                    model.failed = false;
                    model.retry_at = Some(Instant::now() + Duration::from_secs(2));
                    (
                        "attaching",
                        Some("Terminal client exited; retrying the existing session".into()),
                        Some(failures + 1),
                    )
                }
                ExitDecision::Failed(failures) => {
                    model.child_failures = failures;
                    model.failed = true;
                    model.retry_at = None;
                    (
                        "failed",
                        Some("Terminal client failed three consecutive attachment attempts".into()),
                        Some(3),
                    )
                }
                ExitDecision::Unknown(error) => {
                    model.failed = true;
                    model.retry_at = None;
                    (
                        "failed",
                        Some(format!(
                            "Session status is unknown; preserving the last screen: {error}"
                        )),
                        None,
                    )
                }
            };
            drop(model);
            status(&state, phase, message, attempts);
        });
    });
}

#[cfg(test)]
mod remote_tests {
    use super::{Target, remote_attach_command};
    use std::path::Path;

    #[test]
    fn the_bridge_command_is_quoted_and_names_the_host_and_shell() {
        assert_eq!(
            remote_attach_command(
                Path::new("/Applications/My App/riwork-remote"),
                "h-1",
                "s-1"
            ),
            "'/Applications/My App/riwork-remote' attach --desktop h-1 --shell s-1"
        );
        assert_eq!(
            remote_attach_command(Path::new("/opt/riwork-remote"), "h", "it's"),
            "/opt/riwork-remote attach --desktop h --shell 'it'\\''s'"
        );
    }

    #[test]
    fn remote_targets_are_keyed_like_their_saved_tab_and_validated() {
        let target = Target::Remote {
            desktop_id: "host-1".into(),
            shell_id: "shell-9".into(),
        };
        assert!(target.valid());
        assert_eq!(target.key(), "remote:host-1:shell-9");
        assert_eq!(target.local(), None);
        for (desktop_id, shell_id) in [("", "s"), ("h:x", "s"), ("h", "s 1"), ("h", "s\n")] {
            let target = Target::Remote {
                desktop_id: desktop_id.into(),
                shell_id: shell_id.into(),
            };
            assert!(!target.valid(), "{desktop_id:?} {shell_id:?}");
        }
        assert!(
            !Target::Local {
                shell_id: "remote:h:s".into()
            }
            .valid()
        );
    }
}

#[cfg(test)]
mod exit_tests {
    use super::{ExitDecision, exit_decision};

    #[test]
    fn only_authoritative_session_death_is_gone() {
        assert_eq!(exit_decision(Ok(false), 0), ExitDecision::Gone);
        assert_eq!(
            exit_decision(Err("tmux timed out".into()), 0),
            ExitDecision::Unknown("tmux timed out".into())
        );
        assert_eq!(
            exit_decision(Err("unknown shell".into()), 3),
            ExitDecision::Unknown("unknown shell".into())
        );
    }

    #[test]
    fn live_session_retries_are_bounded_without_spawning_replacement_sessions() {
        assert_eq!(exit_decision(Ok(true), 0), ExitDecision::Retry(1));
        assert_eq!(exit_decision(Ok(true), 1), ExitDecision::Retry(2));
        assert_eq!(exit_decision(Ok(true), 2), ExitDecision::Failed(3));
        assert_eq!(exit_decision(Ok(true), 3), ExitDecision::Failed(4));
    }
}
fn resolve_link(state: Arc<State>, hit: Value) {
    let action = hit["action"].as_str().unwrap_or("hover");
    if matches!(action, "open" | "edit") {
        let cache = state.link_cache.lock().unwrap();
        if let Some((request, generation, event)) = &*cache {
            if hit["requestId"].as_u64() == Some(*request)
                && state.generation.load(Ordering::Acquire) == *generation
            {
                let mut event = event.clone();
                event["action"] = json!(action);
                emit(&state, "linkOpen", event);
            }
        }
        return;
    }
    if state.link_busy.swap(true, Ordering::AcqRel) {
        *state.link_pending.lock().unwrap() = Some(hit);
        return;
    }
    let shell = state.model.lock().unwrap().active.clone();
    let generation = state.generation.load(Ordering::Acquire);
    thread::spawn(move || {
        let found = (|| {
            let manager = SessionManager::at(state.home.clone()).ok()?;
            let shell = shell?;
            let session = manager.registered_session(&shell).ok()?;
            let pane = PaneView::parse(&manager.capture_link_view(&shell).ok()?).ok()?;
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let row = hit["row"].as_u64()? as u32;
            let col = hit["col"].as_u64()? as u32;
            let (link, rows) = pane.link_span_at(
                row,
                col,
                &Bases {
                    cwd: &pane.cwd,
                    root: Some(&session.cwd),
                    home: home.as_deref(),
                },
            )?;
            let event = match link {
                Link::Url(url) => json!({"kind":"url","target":url}),
                Link::Path(path) => {
                    json!({"kind":if path.is_dir {"folder"}else{"file"},"target":path.path,"line":path.line,"column":path.col})
                }
            };
            let rows = rows
                .into_iter()
                .map(|r| json!({"row":r.row,"start":r.cols.start,"end":r.cols.end}))
                .collect::<Vec<_>>();
            Some((event, rows))
        })();
        dispatch2::DispatchQueue::main().exec_async(move || {
            state.link_busy.store(false,Ordering::Release);
            if !state.alive.load(Ordering::Acquire)||state.generation.load(Ordering::Acquire)!=generation {return;}
            let rows=found.as_ref().map(|(_,rows)|rows.clone()).unwrap_or_default();
            *state.link_cache.lock().unwrap()=found.as_ref().map(|(event,_)|(hit["requestId"].as_u64().unwrap_or(0),generation,event.clone()));
            let payload=json!({"requestId":hit["requestId"],"rows":rows,"cols":hit["cols"],"rowsCount":hit["rows"]});
            if let Ok(payload)=cstr(payload.to_string()) {unsafe {rw_terminal_set_link(ptr(&state),payload.as_ptr())};}
            let pending=state.link_pending.lock().unwrap().take();
            if let Some(pending)=pending {resolve_link(state,pending);}
        });
    });
}
fn start_maintenance() {
    static START: std::sync::Once = std::sync::Once::new();
    START.call_once(|| {
        thread::spawn(|| {
            let mut config_watch = config::Watch::new(config::roots());
            let mut check_at = Instant::now();
            let mut gate = maintenance::Gate::default();
            let wake = maintenance_wake();
            let mut observed = wake.generation();
            let mut wait = policy::MIN_RECHECK;
            loop {
                observed = wake.wait(observed, wait);
                // Release the registry before config I/O or model locks: AppKit
                // needs it to register new terminal components.
                let list = maintenance::snapshot(states());
                let list = list
                    .into_iter()
                    .filter(|s| s.alive.load(Ordering::Acquire))
                    .collect::<Vec<_>>();
                let has_components = !list.is_empty();
                let changed = if Instant::now() >= check_at {
                    check_at = Instant::now() + Duration::from_secs(2);
                    let changed = config_watch.changed();
                    if changed {
                        CONFIG_REVISION.fetch_add(1, Ordering::AcqRel);
                    }
                    changed
                } else {
                    false
                };
                let now = Instant::now();
                let mut needs_reconcile = false;
                let identity = list
                    .iter()
                    .map(|s| {
                        let m = s.model.lock().unwrap();
                        needs_reconcile |= m.active.is_none()
                            && m.visible
                            && !m.failed
                            && !m.ended
                            && !m.attaching
                            && !m.closing
                            && m.target.is_some()
                            || m.visible
                                && !m.frozen
                                && !m.closing
                                && m.retry_at.is_some_and(|deadline| now >= deadline);
                        (
                            s.id,
                            s.generation.load(Ordering::Acquire),
                            m.visible,
                            m.frozen,
                            m.active.is_some() && !m.closing,
                            m.pinned || m.ended || m.child_exited,
                            m.hidden,
                            m.retry_at,
                        )
                    })
                    .collect::<Vec<_>>();
                let due = needs_reconcile
                    || clock_ms() >= POOL_DUE.load(Ordering::Acquire)
                    || release_schedules()
                        .lock()
                        .unwrap()
                        .values()
                        .any(|schedule| schedule.due(now));
                let identity = (PLACEMENT_REVISION.load(Ordering::Acquire), identity);
                if let Some(pass) = gate.queue(identity, due, changed) {
                    dispatch2::DispatchQueue::main().exec_async(move || {
                        maintain(list, pass.config_changed);
                        drop(pass);
                    });
                }
                let pool_due = POOL_DUE.load(Ordering::Acquire);
                wait = maintenance::wait_duration(
                    has_components,
                    gate.pending(),
                    check_at.saturating_duration_since(Instant::now()),
                    (pool_due != u64::MAX).then(|| {
                        Duration::from_millis(pool_due.saturating_sub(clock_ms()))
                    }),
                );
            }
        });
    });
}
static RELEASE_SCHEDULES: OnceLock<Mutex<BTreeMap<isize, policy::PlanSchedule>>> = OnceLock::new();
fn release_schedules() -> &'static Mutex<BTreeMap<isize, policy::PlanSchedule>> {
    RELEASE_SCHEDULES.get_or_init(Default::default)
}
fn maintain(states: Vec<Arc<State>>, config_changed: bool) {
    if config_changed {
        refresh_theme_projection();
    }
    let mut windows: BTreeMap<isize, Vec<Arc<State>>> = BTreeMap::new();
    for state in states {
        if !state.alive.load(Ordering::Acquire) {
            continue;
        }
        if config_changed {
            let mut m = state.model.lock().unwrap();
            let retained = m.child_exited;
            m.recreate = m.active.is_some() && !retained;
            drop(m);
            if retained {
                update_config(state.clone());
            } else {
                reconcile(state.clone());
            }
        }
        {
            let mut m = state.model.lock().unwrap();
            if m.visible
                && !m.frozen
                && !m.closing
                && m.retry_at
                    .is_some_and(|deadline| Instant::now() >= deadline)
            {
                m.retry_at = None;
                m.recreate = true;
            }
        }
        let view = unsafe { &*(ptr(&state).cast::<NSView>()) };
        let window = view.window().map(|w| w.windowNumber()).unwrap_or(0);
        windows.entry(window).or_default().push(state);
    }
    let mut schedules = release_schedules().lock().unwrap();
    schedules.retain(|window, _| windows.contains_key(window));
    for (window, states) in &windows {
        let frozen = states.iter().any(|s| s.model.lock().unwrap().frozen);
        let panes = states
            .iter()
            .map(|s| {
                let m = s.model.lock().unwrap();
                policy::PaneState {
                    id: s.id,
                    active: if m.visible { 0 } else { 1 },
                    tabs: vec![policy::TabState {
                        id: s.id,
                        attached: m.active.is_some() && !m.closing,
                        pinned: m.pinned || m.ended || m.child_exited,
                        hidden_for: m.hidden.elapsed(),
                    }],
                }
            })
            .collect::<Vec<_>>();
        let plan = schedules.entry(*window).or_default().check(
            &policy::Screen {
                panes: &panes,
                active_pane: 0,
                focus_mode: false,
                frozen,
            },
            Instant::now(),
        );
        if let Some(plan) = &plan {
            if let Some(state) = states.first() {
                trace(
                    state,
                    "release-policy",
                    json!({"windowNumber":window,"release":plan.release,"recheckMs":plan.recheck.map(|wait|wait.as_millis() as u64),"frozen":frozen}),
                );
            }
        }
        for state in states {
            if plan
                .as_ref()
                .is_some_and(|plan| plan.release.contains(&state.id))
            {
                state.model.lock().unwrap().closing = true;
                unsafe { rw_terminal_shutdown(ptr(state)) };
            } else {
                reconcile(state.clone());
            }
        }
    }
    drop(schedules);
    release_pooled(false);
}

#[cfg(feature = "terminal-fixture")]
static TRACE: Mutex<Vec<Value>> = Mutex::new(Vec::new());
#[cfg(feature = "terminal-fixture")]
fn trace(state: &State, event: &str, data: Value) {
    TRACE
        .lock()
        .unwrap()
        .push(json!({"terminalId":state.id,"event":event,"data":data}));
}
#[cfg(not(feature = "terminal-fixture"))]
fn trace(_state: &State, _event: &str, _data: Value) {}

#[cfg(feature = "terminal-fixture")]
pub fn fixture_snapshot() -> Value {
    fixture_snapshot_inner(true)
}
#[cfg(feature = "terminal-fixture")]
pub fn fixture_status() -> Value {
    fixture_snapshot_inner(false)
}
#[cfg(feature = "terminal-fixture")]
fn fixture_snapshot_inner(include_trace: bool) -> Value {
    let list=states().lock().unwrap().iter().filter_map(Weak::upgrade).map(|s| {
        let m=s.model.lock().unwrap();
        let window=if s.alive.load(Ordering::Acquire) {unsafe {&*(ptr(&s).cast::<NSView>())}.window().map(|w|w.windowNumber())} else {None};
        json!({"id":s.id,"shellId":m.active,"target":m.target.as_ref().map(Target::key),"active":m.active.is_some(),"closing":m.closing,"attaching":m.attaching,"visible":m.visible,"pinned":m.pinned,"frozen":m.frozen,"ended":m.ended,"childExited":m.child_exited,"hiddenForMs":m.hidden.elapsed().as_millis() as u64,"alive":s.alive.load(Ordering::Acquire),"windowNumber":window})
    }).collect::<Vec<_>>();
    {
        let mut result = json!({
            "states":list,
            "pending":unsafe { rw_terminal_pending_count() },
            "pooled":POOL.lock().unwrap().len(),
        });
        if include_trace {
            result["trace"] = json!(*TRACE.lock().unwrap());
        }
        result
    }
}
/// Accelerate only the hidden-age clock of a marker-guarded isolated fixture.
/// Visibility still comes from the production NSH layout and native component.
#[cfg(feature = "terminal-fixture")]
pub fn fixture_policy(shell: &str, args: &Value) -> Result<Value, String> {
    let list = states()
        .lock()
        .unwrap()
        .iter()
        .filter_map(Weak::upgrade)
        .filter(|s| s.alive.load(Ordering::Acquire))
        .collect::<Vec<_>>();
    let state = list
        .iter()
        .find(|s| {
            s.model
                .lock()
                .unwrap()
                .target
                .as_ref()
                .and_then(Target::local)
                == Some(shell)
        })
        .ok_or("no fixture terminal target")?;
    {
        let mut m = state.model.lock().unwrap();
        if let Some(age) = args["hiddenForMs"].as_u64() {
            m.hidden = Instant::now()
                .checked_sub(Duration::from_millis(age))
                .ok_or("invalid fixture hidden age")?;
        }
        if let Some(pinned) = args["pinned"].as_bool() {
            m.pinned = pinned;
        }
        if let Some(frozen) = args["frozen"].as_bool() {
            m.frozen = frozen;
        }
    }
    sync_state(state);
    release_schedules().lock().unwrap().clear();
    // Batch age changes before one real policy pass, so tests can exercise the
    // two-release cap without the main-loop timer racing fixture preparation.
    if !args["defer"].as_bool().unwrap_or(false) {
        maintain(list, false);
    }
    Ok(fixture_snapshot())
}

#[cfg(feature = "terminal-fixture")]
pub fn fixture_input(shell: &str, text: &str) -> Result<(), String> {
    use nsh_platform_macos::{
        objc2::msg_send,
        objc2_foundation::{NSRange, NSString},
    };
    let list = states()
        .lock()
        .unwrap()
        .iter()
        .filter_map(Weak::upgrade)
        .collect::<Vec<_>>();
    let state = list
        .into_iter()
        .find(|s| {
            s.alive.load(Ordering::Acquire)
                && s.model.lock().unwrap().active.as_deref() == Some(shell)
        })
        .ok_or("no attached fixture terminal")?;
    let view = unsafe { &*(ptr(&state).cast::<NSView>()) };
    // Runs the production NSTextInputClient commit path in this fixture view.
    // No global/desktop input and no test-only terminal command is registered.
    let text = NSString::from_str(text);
    unsafe {
        let _: () = msg_send![view, insertText:&*text, replacementRange:NSRange::new(usize::MAX,0)];
    }
    Ok(())
}

#[cfg(feature = "terminal-fixture")]
pub fn fixture_command(shell: &str, name: &str, args: Value) -> Result<(), String> {
    let state = states()
        .lock()
        .unwrap()
        .iter()
        .filter_map(Weak::upgrade)
        .find(|s| {
            s.alive.load(Ordering::Acquire)
                && s.model.lock().unwrap().active.as_deref() == Some(shell)
        })
        .ok_or("no attached fixture terminal")?;
    match name {
        "pasteboard" => {
            unsafe extern "C" {
                fn rw_terminal_fixture_pasteboard(view: *mut c_void, json: *const c_char) -> bool;
            }
            let payload = cstr(args.to_string())?;
            if !unsafe { rw_terminal_fixture_pasteboard(ptr(&state), payload.as_ptr()) } {
                return Err("native pasteboard input refused".into());
            }
        }
        "paste" if args.get("text").is_some() => {
            request_input(state, json!({"kind":"text","text":args["text"]}))
        }
        "dropPaths" => request_input(state, json!({"kind":"files","paths":args["paths"]})),
        "pasteImage" => request_input(state, json!({"kind":"image","pngBase64":args["pngBase64"]})),
        "find" => {
            let query = cstr(args["query"].as_str().ok_or("missing find query")?)?;
            unsafe { rw_terminal_find(ptr(&state), query.as_ptr()) };
        }
        "paste" | "copy" | "selectAll" | "findNext" | "findPrevious" | "findClose" => {
            let command = cstr(name)?;
            unsafe { rw_terminal_command(ptr(&state), command.as_ptr()) };
        }
        _ => return Err("unsupported fixture terminal command".into()),
    }
    Ok(())
}

#[cfg(feature = "terminal-fixture")]
pub fn fixture_heartbeat() {
    TRACE.lock().unwrap().push(json!({"terminalId":0,"event":"heartbeat","data":{"monotonicMs":unsafe{rw_terminal_monotonic_ms()}}}));
}

#[cfg(feature = "terminal-fixture")]
pub fn fixture_probe(shell: &str, kind: &str) -> Result<(), String> {
    unsafe extern "C" {
        fn rw_terminal_fixture_probe(view: *mut c_void, kind: *const c_char) -> bool;
    }
    let list = states()
        .lock()
        .unwrap()
        .iter()
        .filter_map(Weak::upgrade)
        .collect::<Vec<_>>();
    let state = list
        .into_iter()
        .find(|s| {
            s.alive.load(Ordering::Acquire)
                && s.model.lock().unwrap().active.as_deref() == Some(shell)
        })
        .ok_or("no attached fixture terminal")?;
    if kind == "stale-drop" {
        // Model an accepted async input completion queued before this view
        // became hidden. This does not read a pasteboard or publish OS input.
        let payload = json!({"accepted":true,"paths":[],"images":[]});
        trace(&state, "drop", payload.clone());
        emit(&state, "drop", payload);
        return Ok(());
    }
    let kind = cstr(kind)?;
    if unsafe { rw_terminal_fixture_probe(ptr(&state), kind.as_ptr()) } {
        Ok(())
    } else {
        Err("unknown or unavailable native probe".into())
    }
}
