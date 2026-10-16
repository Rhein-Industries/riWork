//! Explicit development-only visual QA. Uses in-memory windows and Metal; it
//! cannot start an app process or interact with a live desktop window.

use super::*;
use riwork_engine::app_preview::PreviewLog;
use riwork_gpui_preview::HeadlessPreview;

struct FakeSession;
impl PreviewSession for FakeSession {
    fn snapshot(&self) -> PreviewState {
        fixture_state()
    }
    fn set_visible(&self, _: bool) {}
    fn input(&self, _: PreviewInput) {}
    fn stop(&self) {}
    fn restart(&self) {}
    fn select_window(&self, _: u32, _: u64) {}
}

fn fixture_state() -> PreviewState {
    PreviewState {
        phase: PreviewPhase::Streaming,
        mode: PreviewMode::Cooperative,
        pid: None,
        window_id: None,
        windows: Vec::new(),
        ios: None,
        frame: None,
        warning: None,
        message: "Streaming hidden GPUI app · independent process".into(),
        logs: vec![
            PreviewLog {
                text: "Compiling preview-demo v0.1.0".into(),
                stderr: false,
            },
            PreviewLog {
                text: "Finished dev profile in 0.72s".into(),
                stderr: false,
            },
            PreviewLog {
                text: "Connected to RiWork · rendering without a desktop window".into(),
                stderr: false,
            },
        ],
    }
}

struct Demo;
impl Render for Demo {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p_8()
            .flex()
            .flex_col()
            .gap_5()
            .bg(rgb(0x18202f))
            .text_color(rgb(0xe7edf8))
            .font_family("Helvetica")
            .text_size(px(16.))
            .child(div().text_size(px(28.)).child("Your GPUI app"))
            .child("Rendered offscreen in its own process")
            .child(
                div().flex().gap_4().children(
                    [
                        ("Running", "Preview connected"),
                        ("Windowless", "No Dock icon"),
                    ]
                    .into_iter()
                    .map(|(title, detail)| {
                        div()
                            .flex_1()
                            .rounded_lg()
                            .bg(rgb(0x263248))
                            .p_5()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(div().text_size(px(20.)).child(title))
                            .child(detail)
                    }),
                ),
            )
            .child(
                div()
                    .rounded_md()
                    .bg(rgb(0x3e72e6))
                    .p_4()
                    .w(px(240.))
                    .child("Click and type in RiWork"),
            )
    }
}

pub fn render_fixture_if_requested() -> bool {
    let Some(directory) = std::env::var_os("RIWORK_PREVIEW_UI_SNAPSHOT") else {
        return false;
    };
    if let Err(error) = render(PathBuf::from(directory)) {
        eprintln!("App Preview fixture failed: {error}");
        std::process::exit(1);
    }
    true
}

fn render(directory: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(&directory)?;
    let mut demo = HeadlessPreview::new(800, 500, |_, cx| cx.new(|_| Demo))?;
    let mut png = Cursor::new(Vec::new());
    demo.frame()?.write_to(&mut png, image::ImageFormat::Png)?;
    let frame = PreviewFrame {
        png: png.into_inner(),
        width: 800,
        height: 500,
        sequence: 1,
    };
    for (name, width, height) in [("wide", 1000, 760), ("narrow", 540, 680)] {
        let image = decode_png(&frame)?;
        let mut preview = HeadlessPreview::new(width, height, move |window, app| {
            app.set_global(crate::settings::Settings::default());
            app.set_global(theme::Appearance {
                selected: theme::ThemeChoice::RiWork,
                palette: theme::Palette::RIWORK,
                terminal: None,
                ghostty: None,
                error: None,
            });
            text_input::init(app);
            behavior::init(app);
            let view = app.new(|cx| {
                let spec = PreviewSpec {
                    command: "cargo run -p preview-demo".into(),
                    cwd: "/Projects/preview-demo".into(),
                    mode: PreviewMode::Cooperative,
                    ios: None,
                };
                let mut view = AppPreviewView::with_launcher(
                    spec.clone(),
                    window,
                    cx,
                    Box::new(|_| unreachable!("fixture must never launch a process")),
                );
                view._poll = None;
                view.session = Some(Box::new(FakeSession));
                view.launched_spec = Some(spec);
                view.state = Some(fixture_state());
                view.image = Some(image);
                view.frame_size = Some((800, 500));
                view.image_sequence = Some(1);
                view.show_logs = true;
                view.visible = true;
                view.set_fields_enabled(false, cx);
                view
            });
            app.new(|cx| gpui_kit::base::Root::new(view, window, cx))
        })?;
        let path = directory.join(format!("app-preview-{name}.png"));
        preview.frame()?.save(&path)?;
        eprintln!("App Preview fixture: {}", path.display());
    }
    Ok(())
}
