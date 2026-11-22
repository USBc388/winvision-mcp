use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::time::Duration;

use windows::Win32::Foundation::{HWND, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
};

use crate::events;
use crate::ocr::OcrCtx;
use crate::uia::UiaCtx;

pub struct Ctx {
    pub uia: Option<UiaCtx>,
    pub ocr: Option<OcrCtx>,
    pub event_state: std::sync::Arc<events::EventState>,
    pub last_hwnd: isize,
}

impl Ctx {
    pub fn uia(&mut self) -> Result<&mut UiaCtx, String> {
        if self.uia.is_none() {
            self.uia = Some(UiaCtx::new()?);
        }
        Ok(self.uia.as_mut().unwrap())
    }

    pub fn ocr(&mut self) -> Result<&mut OcrCtx, String> {
        if self.ocr.is_none() {
            self.ocr = Some(OcrCtx::new());
        }
        Ok(self.ocr.as_mut().unwrap())
    }
}

type Job = Box<dyn FnOnce(&mut Ctx) + Send>;

#[derive(Clone)]
pub struct Actor {
    tx: Sender<Job>,
}

impl Actor {
    
    pub fn spawn() -> Actor {
        let (tx, rx) = channel::<Job>();
        std::thread::Builder::new()
            .name("winvision-com".into())
            .spawn(move || {
                unsafe {
                    let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
                    if hr.is_err() && hr != RPC_E_CHANGED_MODE {
                        eprintln!("winvision: CoInitializeEx failed: {hr:?}");
                        return;
                    }
                }
                let event_state = events::install_hooks();
                let mut ctx = Ctx {
                    uia: None,
                    ocr: None,
                    event_state,
                    last_hwnd: 0,
                };
                loop {
                    
                    unsafe {
                        let mut msg = MSG::default();
                        while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).0 != 0 {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                    match rx.recv_timeout(Duration::from_millis(8)) {
                        Ok(job) => run_job(job, &mut ctx),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .expect("failed to spawn COM actor thread");
        Actor { tx }
    }

    pub fn call<R, F>(&self, f: F) -> Result<R, String>
    where
        R: Send + 'static,
        F: FnOnce(&mut Ctx) -> Result<R, String> + Send + 'static,
    {
        let (rtx, rrx) = channel();
        let job: Job = Box::new(move |ctx: &mut Ctx| {
            let _ = rtx.send(f(ctx));
        });
        self.tx
            .send(job)
            .map_err(|_| "winvision COM actor thread is dead".to_string())?;
        match rrx.recv_timeout(Duration::from_secs(120)) {
            Ok(res) => res,
            Err(RecvTimeoutError::Timeout) => {
                Err("winvision operation timed out after 120s".to_string())
            }
            Err(RecvTimeoutError::Disconnected) => Err("winvision COM actor thread died".to_string()),
        }
    }
}

fn run_job(job: Job, ctx: &mut Ctx) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(ctx)));
    if r.is_err() {
        eprintln!("winvision: a job panicked on the COM thread (recovered)");
    }
}
