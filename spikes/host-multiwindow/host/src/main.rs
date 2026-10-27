//! Throwaway AppKit-only host. Every surface belongs to this isolated process.
use nsh_dev::{
    DevClient, DevEvent, HostInfo,
    protocol::{BundleFormat, HostPlatform},
};
use nsh_engine::{
    HostChannel,
    perf::{PerfSnapshot, Stat},
};
use nsh_platform_macos::{
    App, AppOptions, AppView, Appearance, Bundle,
    objc2::{MainThreadMarker, MainThreadOnly, rc::autoreleasepool},
    objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEventMask, NSWindow,
        NSWindowStyleMask,
    },
    objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString},
    set_ui_sets,
};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Write,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

fn option(name: &str, default: &str) -> String {
    let args: Vec<_> = std::env::args().collect();
    args.windows(2)
        .find(|w| w[0] == name)
        .map(|w| w[1].clone())
        .unwrap_or_else(|| default.into())
}
fn stat(s: Stat) -> Value {
    json!({"mean":s.mean.as_secs_f64()*1000.,"p50":s.p50.as_secs_f64()*1000.,
        "p95":s.p95.as_secs_f64()*1000.,"max":s.max.as_secs_f64()*1000.})
}
fn perf(s: PerfSnapshot) -> Value {
    json!({"retainedFrameCount":s.frames.len(), "fps":s.fps, "refreshHz":s.refresh_rate,
        "frameMs":stat(s.frame), "jsMs":stat(s.js), "layoutMs":stat(s.layout),
        "queueLatencyMs":stat(s.latency), "frameIntervalMs":stat(s.frame_interval),
        "tickIntervalMs":stat(s.tick_interval), "commitsPerSecond":s.commits,
        "overBudgetFraction":s.dropped, "engineBusyPercent":s.engine_busy,
        "processCpuPercent":s.process.cpu, "processMemoryBytes":s.process.memory,
        "retainedFramesMs":s.frames.iter().map(|d|d.as_secs_f64()*1000.).collect::<Vec<_>>()})
}
fn record(out: &mut File, value: Value) {
    writeln!(out, "{value}").unwrap();
    out.flush().unwrap();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let output = PathBuf::from(option("--output", "../evidence/run.jsonl"));
    let data = PathBuf::from(option("--data-dir", "../.run/data"));
    fs::create_dir_all(output.parent().unwrap())?;
    fs::create_dir_all(&data)?;
    let mut out = File::create(output)?;
    let rows: u64 = option("--history-rows", "200").parse()?;
    let interval: u64 = option("--interval-ms", "16").parse()?;
    let timeout: u64 = option("--timeout-ms", "90000").parse()?;
    let linger: u64 = option("--linger-ms", "300").parse()?;
    let stop_generation: u64 = option("--exit-after-reloads", "0").parse()?;
    let dev_url = option("--dev-url", "");
    let bundle = if dev_url.is_empty() {
        Some(Bundle::read(&PathBuf::from(option(
            "--bundle",
            "../app/.nsh/out/bundle.js",
        )))?)
    } else {
        None
    };
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();
    set_ui_sets(vec![nsh_base::registry(), riwork_nsh_probe::registry()]);
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();
    let mut windows = Vec::new();
    let mut views = Vec::new();
    for (index, id) in ["A", "B"].into_iter().enumerate() {
        let channel = HostChannel::new();
        channel.set_params(
            json!({"windowId":id,"label":format!("Isolated window {id}"),
            "historyRows":rows,"chunkBytes":40,"intervalMs":interval})
            .as_object()
            .unwrap()
            .clone(),
        );
        for topic in ["ready", "progress", "done", "probe", "error"] {
            let tx = tx.clone();
            channel.handle(topic, move |mut call| {
                let _ = tx.send((index, topic, started.elapsed(), call.take_data()));
            });
        }
        let view = AppView::new(
            mtm,
            AppOptions {
                app_id: format!("dev.riwork.nsh.fixture.{id}"),
                title: format!("NSH fixture {id}"),
                size: (720, 720),
                appearance: Appearance::Light,
                data_dir: Some(data.join(id)),
                host: channel,
                fps: true,
                ..Default::default()
            },
        );
        // Each NSWindow owns a different NSView and engine, with the same bundle source.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(
                    NSPoint::new(40. + index as f64 * 740., 100.),
                    NSSize::new(720., 720.),
                ),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(&format!(
            "ISOLATED NSH 0.3 fixture — {id}"
        )));
        window.setContentView(Some(&view.view()));
        window.orderFront(None);
        if let Some(bundle) = &bundle {
            view.load(App::Js(bundle.clone()));
        }
        record(
            &mut out,
            json!({"kind":"window","windowId":id,"pid":std::process::id(),
            "nativeWindowNumber":window.windowNumber(),"params":view.host().params(),"backend":"AppKit"}),
        );
        windows.push(window);
        views.push(view);
    }
    let (dev_tx, dev_rx) = mpsc::channel();
    let _dev_client = if dev_url.is_empty() {
        None
    } else {
        Some(DevClient::start(
            dev_url.clone(),
            HostInfo {
                device: "RiWork isolated two-window fixture".into(),
                platform: HostPlatform::MacOs,
                // Ask for JS source: no cross-version QuickJS bytecode assumption.
                bytecode_version: "source-only-fixture".into(),
                ui_sets: vec!["nsh/base".into(), "riwork/native".into()],
                tests: false,
            },
            move |event| {
                let _ = dev_tx.send(event);
            },
        )?)
    };
    let mut ready = [false; 2];
    let mut done = [false; 2];
    let mut last_ready = Instant::now();
    let mut replay_started = None;
    let mut done_at = None;
    let mut settled = false;
    let mut next_sample = Instant::now();
    let mut generation = 0;
    while started.elapsed() < Duration::from_millis(timeout) {
        autoreleasepool(|_| {
            let until = NSDate::dateWithTimeIntervalSinceNow(0.005);
            if let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&until),
                unsafe { NSDefaultRunLoopMode },
                true,
            ) {
                app.sendEvent(&event);
            }
            app.updateWindows();
        });
        for event in dev_rx.try_iter() {
            match event {
                DevEvent::Bundle {
                    version,
                    format,
                    bytes,
                    capabilities,
                    app_id,
                    source_map,
                    ..
                } => {
                    if format != BundleFormat::Source {
                        return Err("dev server ignored source negotiation".into());
                    }
                    let source = String::from_utf8(bytes)?;
                    let source_bytes = source.len();
                    let mut bundle = Bundle::source(source)
                        .with_app_id(app_id.as_deref())
                        .with_capabilities_json(capabilities.as_deref())?;
                    if let Some(map) = source_map {
                        bundle = bundle.with_source_map(map);
                    }
                    generation += 1;
                    ready = [false; 2];
                    done = [false; 2];
                    replay_started = None;
                    done_at = None;
                    settled = false;
                    for view in &views {
                        view.load(App::Js(bundle.clone()));
                    }
                    record(
                        &mut out,
                        json!({"kind":"devReload","version":version,"generation":generation,
                            "mode":"full-remount","sourceBytes":source_bytes,"hostMs":started.elapsed().as_secs_f64()*1000.}),
                    );
                }
                DevEvent::Patch { version, .. } => record(
                    &mut out,
                    json!({"kind":"devPatchNotice","version":version,"action":"wait-for-full-bundle; AppView has no public hot-update method"}),
                ),
                DevEvent::BuildFailed(message) | DevEvent::CannotRun { message, .. } => {
                    record(&mut out, json!({"kind":"devError","message":message}));
                    return Err(message.into());
                }
                _ => {}
            }
        }
        for (index, topic, elapsed, payload) in rx.try_iter() {
            if topic == "ready" {
                ready[index] = true;
                last_ready = Instant::now();
            }
            if topic == "done" {
                done[index] = true;
            }
            record(
                &mut out,
                json!({"kind":"hostEvent","index":index,"topic":topic,"payload":payload,
                "generation":generation,"hostMs":elapsed.as_secs_f64()*1000.}),
            );
        }
        if ready.iter().all(|b| *b)
            && replay_started.is_none()
            && last_ready.elapsed() > Duration::from_millis(500)
        {
            for view in &views {
                view.host().send("replay", json!({}))?;
            }
            replay_started = Some(Instant::now());
            record(
                &mut out,
                json!({"kind":"replayStart","generation":generation,"hostMs":started.elapsed().as_secs_f64()*1000.}),
            );
        }
        if Instant::now() >= next_sample {
            next_sample = Instant::now() + Duration::from_secs(1);
            for (i, view) in views.iter().enumerate() {
                if let Some(m) = view.perf_monitor() {
                    record(
                        &mut out,
                        json!({"kind":"perfSample","index":i,"generation":generation,"hostMs":started.elapsed().as_secs_f64()*1000.,"perf":perf(m.snapshot())}),
                    );
                }
            }
        }
        if done.iter().all(|b| *b) && done_at.is_none() {
            done_at = Some(Instant::now());
            for (i, view) in views.iter().enumerate() {
                record(
                    &mut out,
                    json!({"kind":"perfDone","index":i,"generation":generation,"hostMs":started.elapsed().as_secs_f64()*1000.,"perf":perf(view.perf_monitor().unwrap().snapshot())}),
                );
            }
        }
        if !settled && done_at.is_some_and(|t| t.elapsed() >= Duration::from_millis(250)) {
            for (i, view) in views.iter().enumerate() {
                record(
                    &mut out,
                    json!({"kind":"perfSettled","index":i,"generation":generation,
                    "hostMs":started.elapsed().as_secs_f64()*1000.,
                    "perf":perf(view.perf_monitor().unwrap().snapshot())}),
                );
            }
            settled = true;
        }
        if (dev_url.is_empty() || (stop_generation > 0 && generation >= stop_generation))
            && done_at.is_some_and(|t| t.elapsed() >= Duration::from_millis(linger))
        {
            break;
        }
    }
    let success = done.iter().all(|b| *b);
    record(
        &mut out,
        json!({"kind":"exit","success":success,"hostMs":started.elapsed().as_secs_f64()*1000.,"verification_environment":"isolated fixtures only"}),
    );
    for view in &views {
        view.stop();
    }
    for window in &windows {
        window.orderOut(None);
    }
    if !success {
        return Err("fixture timed out without both done events".into());
    }
    Ok(())
}
