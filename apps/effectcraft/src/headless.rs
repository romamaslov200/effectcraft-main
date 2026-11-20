//! The render service: EffectCraft without a window.
//!
//! A host that links compositions needs their frames, not a running editor. This mode opens the
//! project, answers the control channel and renders the frames asked for — the same code the
//! window would run, with nothing drawn. What a window is needed for (a screenshot, synthetic
//! input, a job awaited across frames) is answered as unavailable, so a caller learns the
//! difference instead of waiting.

use effectcraft_ui_egui::control::{self, Outcome};
use effectcraft_ui_egui::{ControlRequest, EffectcraftApp};
use serde_json::json;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// How long a request that asks for another frame may wait before it is answered as failed; the
/// window's own loop uses the same deadline.
const RETRY_SECONDS: f64 = 3.0;
/// With no window there is nobody to press a button: a service that sat unused this long is
/// closed, and the next frame starts a fresh one.
const IDLE_EXIT: Duration = Duration::from_secs(1800);

/// Run the service: open `project` (when given), listen on `port`, answer until asked to quit.
/// `watch_stdin` ends it when the process that started it goes away — the host holds the write end
/// of the pipe, so a dead host is a closed pipe, however it died.
pub fn run(port: u16, project: Option<String>, watch_stdin: bool) -> i32 {
    let mut session = effectcraft_host::session();
    // Fast open: the frames are what is wanted, and footage is checked when it is used.
    session.check_footage_on_open = false;
    if let Some(dir) = effectcraft_host::config_dir() {
        session.config = Some(std::sync::Arc::new(effectcraft_engine::config::DirConfig::new(dir)));
        session.load_settings();
    }
    if let Some(path) = &project {
        if let Err(e) = session.execute("file.open", json!({ "path": path })) {
            eprintln!("effectcraft: cannot open {path}: {e}");
            return 2;
        }
        log::info!("render service: opened {path}");
    }
    attach_compositor(&mut session);
    let mut app = EffectcraftApp::new(session);
    let ctx = egui::Context::default();
    let rx = super::control_server::start(port, ctx.clone());
    let parent_gone = watch_stdin.then(|| {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let gone = flag.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 64];
            let mut stdin = std::io::stdin();
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        gone.store(true, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                    Ok(_) => {}
                }
            }
        });
        flag
    });
    log::info!("render service: listening on 127.0.0.1:{port}");
    let start = Instant::now();
    let mut deferred: Vec<(ControlRequest, f64)> = Vec::new();
    let mut last_asked = Instant::now();
    loop {
        let now = start.elapsed().as_secs_f64();
        let mut quit = false;
        let mut reqs: Vec<(ControlRequest, f64)> = std::mem::take(&mut deferred);
        while let Ok(req) = rx.try_recv() {
            reqs.push((req, now + RETRY_SECONDS));
        }
        for (req, deadline) in reqs {
            last_asked = Instant::now();
            let reply = req.reply.clone();
            let asked_to_quit = req.method == "app.quit";
            match control::handle(&mut app, &ctx, &req, now) {
                Outcome::Done(v) => {
                    let _ = reply.send(v);
                    quit |= asked_to_quit;
                }
                // "Not on screen yet" in a window; here nothing ever will be, so the deadline is
                // what is left to wait for.
                Outcome::Retry(msg) => {
                    if now < deadline {
                        deferred.push((req, deadline));
                    } else {
                        let _ = reply.send(json!({"ok": false, "error": msg}));
                    }
                }
                Outcome::AfterInput | Outcome::Screenshot { .. } => {
                    let _ = reply.send(json!({"ok": false, "error": "the render service has no window"}));
                }
                // A job awaited across frames needs the window's ticks to see it end; the command's
                // own immediate result is what a `wait: false` caller gets, and what is sent.
                Outcome::AwaitJobs(w) => {
                    let _ = reply.send(w.result);
                }
                Outcome::AwaitScript(_) => {
                    let _ = reply.send(json!({"ok": false, "error": "the render service cannot yield a script to a renderer"}));
                }
            }
        }
        let gone = parent_gone.as_ref().is_some_and(|g| g.load(std::sync::atomic::Ordering::Relaxed));
        if quit || gone || last_asked.elapsed() > IDLE_EXIT {
            log::info!("render service: closing");
            return 0;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
}

/// Where the service is told to run. Kept out of [`run`] so the argument rules read on their own.
pub fn port_of(control: Option<u16>) -> Result<u16, String> {
    control.ok_or_else(|| "--headless needs --control <port>: the port the host connects to".to_owned())
}
/// Settings ▸ Performance ▸ Render Device: the frames this service hands out render on the GPU
/// compositor unless the user chose the CPU. The window borrows eframe's device; a service has no
/// window, so it makes one of its own — without this every linked preview (FilmCraft asking for
/// the frames) rendered on the CPU, whatever the project's renderer said.
fn attach_compositor(session: &mut effectcraft_engine::Session) {
    let device = session.prefs.performance.render_device.clone();
    if !session.gpu_allowed() {
        session.accel_note = Some("Render Device is CPU (Settings ▸ Performance)".into());
        log::info!("render service: Render Device is CPU; frames render on the CPU");
        return;
    }
    // `nvidia` names the card CUDA would run on; the compositor drives it through D3D12/Metal.
    let pick = if device.eq_ignore_ascii_case("nvidia") { effectcraft_gpu::AdapterPick::Vendor(0x10DE) } else { effectcraft_gpu::AdapterPick::Best };
    match effectcraft_gpu::Gpu::try_headless_pick(pick) {
        Ok(g) => {
            session.accel = Some(std::sync::Arc::new(g));
            let name = session.accel.as_ref().map(|a| a.name()).unwrap_or_default();
            log::info!("render service: GPU compositor on {name}");
        }
        Err(e) => {
            log::warn!("render service: no GPU compositor ({e}); frames render on the CPU");
            session.accel_note = Some(format!("no GPU compositor ({e})"));
        }
    }
}
