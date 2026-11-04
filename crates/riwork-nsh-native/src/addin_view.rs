//! `riwork.addin`: an add-in's view, an NSH engine of its own embedded in a Somtus tab.
//!
//! The engine is loaded without Somtus's native modules: its UI sets are `nsh/base` only, so
//! neither an import nor `nativeModule("riwork/native")` reaches Somtus. The add-in reaches
//! Somtus through the add-in API on its host channel (addins.rs), for what it was granted, and
//! its bundle's capabilities allow those host calls and its network origins only
//! (addin_build.rs). Nothing loads until the add-in is trusted; a source change rebuilds and
//! reloads it while it shows (an agent writing it, live), and a change to what it asks for
//! stops it until it is trusted again.
use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    rc::{Rc, Weak},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use nsh_core::JsonValue;
use nsh_engine::HostChannel;
use nsh_platform_macos::{
    App, AppOptions, AppView, Bundle, CacheLocation, CacheOptions, ComponentContext, Components,
    NativeComponent,
    objc2::{MainThreadMarker, MainThreadOnly, rc::Retained},
    objc2_app_kit::{NSAutoresizingMaskOptions, NSView},
    objc2_foundation::{NSPoint, NSRect, NSSize},
    set_ui_sets,
};
use riwork_core::addins::{self, Found};
use riwork_engine::addin_build;
use serde_json::json;

use crate::{addins::Api, service};

static NEXT: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static VIEWS: RefCell<HashMap<u64, Weak<RefCell<Inner>>>> = RefCell::default();
}

/// Runs `work` on the main thread with the view `id`, if it is still there.
fn on_main(id: u64, work: impl FnOnce(&Rc<RefCell<Inner>>) + Send + 'static) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        let inner = VIEWS.with(|views| views.borrow().get(&id).and_then(Weak::upgrade));
        if let Some(inner) = inner {
            work(&inner);
        }
    });
}

/// Register before constructing the first AppView, beside riwork.preview.
pub fn register(components: &Components) {
    components.register(
        "riwork.addin",
        Rc::new(move |context| {
            let mtm = MainThreadMarker::new().expect("components are made on the main thread");
            let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 300.0));
            let container = NSView::initWithFrame(NSView::alloc(mtm), frame);
            container.setAutoresizesSubviews(true);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let inner = Rc::new(RefCell::new(Inner {
                id, context, container: container.clone(), addin: String::new(), view: String::new(),
                project: String::new(), args: JsonValue::Null, palette: JsonValue::Null,
                workspace: JsonValue::Null,
                app: None, api: None, watch: None, generation: 0, start_pending: false,
                rebuilding: Arc::new(AtomicBool::new(false)),
            }));
            VIEWS.with(|views| views.borrow_mut().insert(id, Rc::downgrade(&inner)));
            Box::new(AddinView { container, inner })
        }),
    );
}

struct AddinView {
    container: Retained<NSView>,
    inner: Rc<RefCell<Inner>>,
}

struct Inner {
    id: u64,
    context: ComponentContext,
    container: Retained<NSView>,
    addin: String,
    view: String,
    project: String,
    args: JsonValue,
    palette: JsonValue,
    workspace: JsonValue,
    app: Option<Rc<AppView>>,
    api: Option<Arc<Api>>,
    watch: Option<notify::RecommendedWatcher>,
    /// Each start counts; a build that finishes after a newer start is dropped.
    generation: u64,
    start_pending: bool,
    rebuilding: Arc<AtomicBool>,
}

fn state(inner: &Inner, status: &str, extra: serde_json::Value) {
    let mut data = json!({"status": status});
    if let (Some(data), Some(extra)) = (data.as_object_mut(), extra.as_object()) {
        data.extend(extra.clone());
    }
    inner.context.emit("state", data);
}

impl Inner {
    /// Stops what runs: its engine, workers and source watch.
    fn close(&mut self) {
        self.watch = None;
        if let Some(api) = self.api.take() {
            api.close();
        }
        if let Some(app) = self.app.take() {
            app.stop();
            app.view().removeFromSuperview();
        }
    }

    fn schedule_start(this: &Rc<RefCell<Self>>) {
        let mut inner = this.borrow_mut();
        if inner.start_pending {
            return;
        }
        inner.start_pending = true;
        // Props arrive one by one right after the view is made: start once they have.
        on_main(inner.id, |this| Inner::start(this));
    }

    fn start(this: &Rc<RefCell<Self>>) {
        let mut inner = this.borrow_mut();
        inner.start_pending = false;
        if inner.addin.is_empty() || inner.view.is_empty() {
            return;
        }
        inner.close();
        inner.generation += 1;
        let (id, generation, project, key) = (inner.id, inner.generation, inner.project.clone(), inner.addin.clone());
        state(&inner, "loading", json!({}));
        drop(inner);
        std::thread::Builder::new()
            .name("riwork-addin-build".into())
            .spawn(move || {
                let prepared = prepare(&project, &key);
                on_main(id, move |this| match prepared {
                    Err(Prepared::Untrusted(found)) => {
                        let inner = this.borrow();
                        if inner.generation == generation {
                            state(&inner, "untrusted", json!({"key": found.key()}));
                        }
                    }
                    Err(Prepared::Failed(error)) => {
                        let inner = this.borrow();
                        if inner.generation == generation {
                            state(&inner, "failed", json!({"error": error}));
                        }
                    }
                    Ok((found, bundle, digest)) => Inner::load(this, generation, found, bundle, digest),
                });
            })
            .ok();
    }

    /// Runs `bundle` in the view's engine, making it the first time.
    fn load(this: &Rc<RefCell<Self>>, generation: u64, found: Found, bundle: PathBuf, digest: String) {
        let mut inner = this.borrow_mut();
        if inner.generation != generation {
            return;
        }
        let bundle = match Bundle::read(&bundle) {
            Ok(bundle) => bundle,
            Err(error) => return state(&inner, "failed", json!({"error": format!("could not read the add-in's bundle: {error}")})),
        };
        if inner.app.is_none() {
            match inner.make_engine(&found) {
                Ok(app) => inner.app = Some(app),
                Err(error) => return state(&inner, "failed", json!({"error": error})),
            }
        }
        let app = inner.app.clone().unwrap();
        // Only nsh/base for this engine: an add-in never gets riwork/native, by import or by
        // nativeModule(). Every engine reads the UI sets when it loads.
        set_ui_sets(vec![nsh_base::registry()]);
        app.load(App::Js(bundle));
        set_ui_sets(crate::ui_sets());
        state(&inner, "running", json!({"digest": digest}));
        if inner.watch.is_none() && found.dir.join("App.tsx").is_file() {
            inner.watch = watch(inner.id, &found, inner.rebuilding.clone());
        }
    }

    fn make_engine(&mut self, found: &Found) -> Result<Rc<AppView>, String> {
        let home = service::process_home()?;
        let manifest = found.manifest.as_ref().map_err(Clone::clone)?;
        let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
        let channel = HostChannel::new();
        let view = manifest.views.iter().find(|view| view.id == self.view).ok_or_else(|| format!("{} has no view {:?}.", manifest.name, self.view))?;
        let scope = match found.scope { addins::Scope::Project { .. } => "project", addins::Scope::Personal => "personal" };
        let params = json!({"addin": {
            "id": manifest.id, "name": manifest.name, "version": manifest.version, "scope": scope,
            "view": view.id, "args": self.args, "palette": self.palette,
        }});
        channel.set_params(params.as_object().cloned().unwrap_or_default());
        let id = self.id;
        let emit = Box::new(move |name: &str, data: serde_json::Value| {
            let name = name.to_owned();
            on_main(id, move |this| this.borrow().context.emit(&name, data));
        });
        let api = Api::new(home.clone(), found, self.project.clone(), channel.clone(), emit, self.workspace.clone())?;
        api.serve();
        self.api = Some(api);
        let data = addins::data_dir(&home, &found.scope, &manifest.id);
        let bounds = self.container.bounds();
        let app = AppView::new(mtm, AppOptions {
            app_id: addin_build::nsh_id(&found.scope, &manifest.id),
            title: manifest.name.clone(),
            size: (bounds.size.width.max(1.0) as i32, bounds.size.height.max(1.0) as i32),
            data_dir: Some(data.join("nsh")),
            cache: CacheOptions::at(CacheLocation::Dir(data.join("nsh/cache"))),
            host: channel,
            components: Components::default(),
            ..Default::default()
        });
        let view = app.view();
        view.setFrame(bounds);
        view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
        self.container.addSubview(&view);
        Ok(app)
    }
}

enum Prepared {
    Untrusted(Found),
    Failed(String),
}

/// Finds, checks trust, and builds: off the main thread.
fn prepare(project: &str, key: &str) -> Result<(Found, PathBuf, String), Prepared> {
    let home = service::process_home().map_err(Prepared::Failed)?;
    let found = crate::addins::find(&home, project, key).map_err(Prepared::Failed)?;
    if let Err(problem) = &found.manifest {
        return Err(Prepared::Failed(problem.clone()));
    }
    if !crate::addins::is_trusted(&home, &found) {
        return Err(Prepared::Untrusted(found));
    }
    let (bundle, digest) = crate::addins::bundle(&home, &found).map_err(Prepared::Failed)?;
    Ok((found, bundle, digest))
}

/// Rebuilds and reloads while the add-in's source changes; a burst of saves rebuilds once.
fn watch(id: u64, found: &Found, rebuilding: Arc<AtomicBool>) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher;
    let key = found.key();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        let ignored = |path: &std::path::Path| path.components().any(|part| [".bundle", ".nsh", "out", "node_modules", ".git"].contains(&part.as_os_str().to_string_lossy().as_ref()));
        if event.paths.iter().all(|path| ignored(path)) || rebuilding.swap(true, Ordering::AcqRel) {
            return;
        }
        let rebuilding = rebuilding.clone();
        let key = key.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            rebuilding.store(false, Ordering::Release);
            on_main(id, move |this| {
                let (project, generation) = { let inner = this.borrow(); (inner.project.clone(), inner.generation) };
                std::thread::spawn(move || {
                    let prepared = prepare(&project, &key);
                    on_main(id, move |this| match prepared {
                        // What it asks for changed: it stops until trusted again.
                        Err(Prepared::Untrusted(found)) => {
                            let mut inner = this.borrow_mut();
                            if inner.generation == generation {
                                inner.close();
                                state(&inner, "untrusted", json!({"key": found.key()}));
                            }
                        }
                        // A failed rebuild keeps the running version and says why.
                        Err(Prepared::Failed(error)) => {
                            let inner = this.borrow();
                            if inner.generation == generation {
                                state(&inner, if inner.app.is_some() { "build-failed" } else { "failed" }, json!({"error": error}));
                            }
                        }
                        Ok((found, bundle, digest)) => Inner::load(this, generation, found, bundle, digest),
                    });
                });
            });
        });
    })
    .ok()?;
    watcher.watch(&found.dir, notify::RecursiveMode::Recursive).ok()?;
    Some(watcher)
}

impl NativeComponent for AddinView {
    fn view(&self) -> Retained<NSView> {
        self.container.clone()
    }

    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        let text = || value.and_then(JsonValue::as_str).unwrap_or_default().to_owned();
        let restart = {
            let mut inner = self.inner.borrow_mut();
            match name {
                "addin" => { let next = text(); let changed = inner.addin != next; inner.addin = next; changed }
                "view" => { let next = text(); let changed = inner.view != next; inner.view = next; changed }
                "project" => { let next = text(); let changed = inner.project != next; inner.project = next; changed }
                "args" => { inner.args = value.cloned().unwrap_or(JsonValue::Null); inner.app.is_none() }
                "palette" => {
                    let next = value.cloned().unwrap_or(JsonValue::Null);
                    if inner.palette != next {
                        inner.palette = next;
                        if let Some(api) = &inner.api { api.palette_changed(&inner.palette); }
                    }
                    false
                }
                // Context is live data, not part of the embedded engine's identity.
                "context" => {
                    inner.workspace = value.cloned().unwrap_or(JsonValue::Null);
                    if let Some(api) = &inner.api {
                        // Project identity changes already queued an engine replacement.
                        // Do not send the new project's tabs through the outgoing API.
                        if api.project_id == inner.project { api.set_workspace(&inner.workspace); }
                    }
                    false
                }
                _ => false,
            }
        };
        if restart {
            Inner::schedule_start(&self.inner);
        }
    }

    fn command(&mut self, name: &str, args: &JsonValue) {
        match name {
            // After trusting it, or after a failure.
            "retry" => Inner::start(&self.inner),
            // One of the add-in's commands, chosen in Somtus.
            "command" => {
                if let Some(api) = &self.inner.borrow().api {
                    let _ = api.channel.send("addin.command", json!({"id": args.get("id").cloned().unwrap_or_default()}));
                }
            }
            _ => {}
        }
    }
}

impl Drop for AddinView {
    fn drop(&mut self) {
        let id = self.inner.borrow().id;
        self.inner.borrow_mut().close();
        VIEWS.with(|views| views.borrow_mut().remove(&id));
    }
}
