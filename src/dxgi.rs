//! DXGI Desktop Duplication screen capture — frames come straight from the
//! GPU compositor (< 5 ms after the first acquire). One duplication per
//! output is kept alive in the COM actor's Ctx and recreated on
//! access-loss / mode change / output switch.

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_FLAG, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication,
    DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Foundation::{HMODULE, RECT};

/// BGRA frame in physical pixels. `origin` = screen coordinates of the
/// top-left corner (so callers can map element rects into image space).
pub struct Frame {
    pub bgra: Vec<u8>,
    pub w: u32,
    pub h: u32,
    pub origin: (i32, i32),
}

pub struct OutputInfo {
    pub adapter: usize,
    pub output: usize,
    pub name: String,
    pub rect: RECT,
}

#[derive(Default)]
pub struct DxgiCtx {
    dup: Option<DupState>,
}

// Desktop Duplication is effectively exclusive per output on many display
// drivers, so the whole process shares ONE duplication context (production
// has a single COM actor; parallel test actors contend on this mutex).
static DXGI_CTX: std::sync::OnceLock<std::sync::Mutex<DxgiCtx>> = std::sync::OnceLock::new();

/// Run `f` with the process-wide DXGI context (serialized access).
pub fn with<T>(f: impl FnOnce(&mut DxgiCtx) -> Result<T, String>) -> Result<T, String> {
    let mutex = DXGI_CTX.get_or_init(|| std::sync::Mutex::new(DxgiCtx::new()));
    let mut guard = mutex
        .lock()
        .map_err(|_| "dxgi context mutex poisoned".to_string())?;
    f(&mut guard)
}

struct DupState {
    duplication: IDXGIOutputDuplication,
    context: ID3D11DeviceContext,
    staging: ID3D11Texture2D,
    w: i32,
    h: i32,
    origin: (i32, i32),
    key: (usize, usize),
}

impl DxgiCtx {
    pub fn new() -> DxgiCtx {
        DxgiCtx { dup: None }
    }

    /// Enumerate all attached outputs (adapter idx, output idx, name, rect).
    pub fn outputs(&self) -> Result<Vec<OutputInfo>, String> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1::<IDXGIFactory1>()
                .map_err(|e| format!("CreateDXGIFactory1 failed: {e}"))?;
            let mut out = Vec::new();
            let mut ai = 0u32;
            loop {
                let adapter: IDXGIAdapter1 = match factory.EnumAdapters1(ai) {
                    Ok(a) => a,
                    Err(_) => break,
                };
                let mut oi = 0u32;
                loop {
                    let output = match adapter.EnumOutputs(oi) {
                        Ok(o) => o,
                        Err(_) => break,
                    };
                    if let Ok(desc) = output.GetDesc() {
                        if desc.AttachedToDesktop.0 != 0 {
                            let name = String::from_utf16_lossy(
                                &desc.DeviceName.iter().take_while(|&&c| c != 0).cloned().collect::<Vec<_>>(),
                            );
                            out.push(OutputInfo {
                                adapter: ai as usize,
                                output: oi as usize,
                                name,
                                rect: desc.DesktopCoordinates,
                            });
                        }
                    }
                    oi += 1;
                }
                ai += 1;
            }
            if out.is_empty() {
                return Err("no attached DXGI outputs found".into());
            }
            Ok(out)
        }
    }

    fn pick_output(&self, center: (i32, i32)) -> Result<(usize, usize, RECT), String> {
        let outs = self.outputs()?;
        let hit = outs.iter().find(|o| {
            let r = &o.rect;
            center.0 >= r.left && center.0 < r.right && center.1 >= r.top && center.1 < r.bottom
        });
        let chosen = hit
            .or_else(|| outs.first())
            .ok_or_else(|| "no output contains the target region".to_string())?;
        Ok((chosen.adapter, chosen.output, chosen.rect))
    }

    fn create_dup(&mut self, adapter_idx: usize, output_idx: usize) -> Result<(), String> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1::<IDXGIFactory1>()
                .map_err(|e| format!("CreateDXGIFactory1 failed: {e}"))?;
            let adapter: IDXGIAdapter1 = factory
                .EnumAdapters1(adapter_idx as u32)
                .map_err(|e| format!("EnumAdapters1({adapter_idx}) failed: {e}"))?;
            let output = adapter
                .EnumOutputs(output_idx as u32)
                .map_err(|e| format!("EnumOutputs({output_idx}) failed: {e}"))?;
            let desc = output
                .GetDesc()
                .map_err(|e| format!("output.GetDesc failed: {e}"))?;
            let origin = (
                desc.DesktopCoordinates.left,
                desc.DesktopCoordinates.top,
            );

            let output1: IDXGIOutput1 = output
                .cast::<IDXGIOutput1>()
                .map_err(|e| format!("QI(IDXGIOutput1) failed: {e}"))?;
            let adapter_for_device = adapter
                .cast::<windows::Win32::Graphics::Dxgi::IDXGIAdapter>()
                .map_err(|e| format!("QI(IDXGIAdapter) failed: {e}"))?;
            let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
            let mut device_opt: Option<windows::Win32::Graphics::Direct3D11::ID3D11Device> = None;
            let mut fl = D3D_FEATURE_LEVEL_11_0;
            let mut context_opt: Option<ID3D11DeviceContext> = None;
            D3D11CreateDevice(
                Some(&adapter_for_device),
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device_opt),
                Some(&mut fl),
                Some(&mut context_opt),
            )
            .map_err(|e| format!("D3D11CreateDevice failed: {e}"))?;
            let device = device_opt.ok_or("D3D11CreateDevice returned no device")?;
            let context = context_opt.ok_or("D3D11CreateDevice returned no context")?;

            let duplication = output1
                .DuplicateOutput(&device)
                .map_err(|e| format!("DuplicateOutput failed: {e} (desktop duplication may be blocked)"))?;
            let dup_desc = duplication.GetDesc();
            let w = dup_desc.ModeDesc.Width as i32;
            let h = dup_desc.ModeDesc.Height as i32;

            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: w as u32,
                Height: h as u32,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging_opt: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging_opt))
                .map_err(|e| format!("CreateTexture2D(staging) failed: {e}"))?;
            let staging = staging_opt.ok_or("CreateTexture2D returned no texture")?;

            self.dup = Some(DupState {
                duplication,
                context,
                staging,
                w,
                h,
                origin,
                key: (adapter_idx, output_idx),
            });
            Ok(())
        }
    }

    /// Grab one frame of the output containing `center` (screen pixels).
    /// DXGI first; if the driver refuses to deliver frames (a fully static
    /// desktop under an animated-wallpaper app can produce NO duplication
    /// frames on NVIDIA) fall back to a GDI BitBlt capture.
    pub fn capture_for_point(&mut self, center: (i32, i32)) -> Result<Frame, String> {
        let (ai, oi, rect) = self.pick_output(center)?;
        self.ensure_dup(ai, oi)?;
        match self.grab() {
            Ok(f) => Ok(f),
            Err(first) => {
                // WAIT_TIMEOUT = static desktop (e.g. animated-wallpaper app
                // paused behind a focused window): NVIDIA delivers no
                // duplication frames at all, so retrying is wasted time —
                // try ONE fast recreate then go straight to GDI.
                let static_screen = first.contains("frame timeout");
                let delays: &[u64] = if static_screen { &[80] } else { &[80, 300] };
                let mut last = first.clone();
                for delay_ms in delays {
                    std::thread::sleep(std::time::Duration::from_millis(*delay_ms));
                    if let Err(e) = self.create_dup(ai, oi) {
                        last = e;
                        continue;
                    }
                    match self.grab() {
                        Ok(f) => return Ok(f),
                        Err(e) => last = e,
                    }
                }
                // Static-screen / driver-stubborn case: GDI never stalls.
                match self.capture_gdi(rect) {
                    Ok(f) => Ok(f),
                    Err(gdi_err) => Err(format!(
                        "capture failed: {last} (first attempt: {first}); GDI fallback also failed: {gdi_err}"
                    )),
                }
            }
        }
    }

    /// GDI BitBlt capture of a screen rect (always works, ~30-100 ms).
    fn capture_gdi(&self, rect: RECT) -> Result<Frame, String> {
        use windows::core::w;
        use windows::Win32::Graphics::Gdi::{
            BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDCW, DeleteDC, DeleteObject,
            GetDIBits, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS,
            SRCCOPY,
        };
        let w = (rect.right - rect.left).max(1) as u32;
        let h = (rect.bottom - rect.top).max(1) as u32;
        unsafe {
            let hdc_screen: windows::Win32::Graphics::Gdi::HDC =
                CreateDCW(w!("DISPLAY"), None, None, None);
            let hdc_mem = CreateCompatibleDC(hdc_screen);
            let hbmp = CreateCompatibleBitmap(hdc_screen, w as i32, h as i32);
            let old = SelectObject(hdc_mem, hbmp);
            let blit = BitBlt(
                hdc_mem,
                0,
                0,
                w as i32,
                h as i32,
                hdc_screen,
                rect.left,
                rect.top,
                SRCCOPY | CAPTUREBLT,
            );
            let mut bi = BITMAPINFO::default();
            bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w as i32;
            bi.bmiHeader.biHeight = -(h as i32); // top-down rows
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            bi.bmiHeader.biCompression = BI_RGB.0;
            let mut buf = vec![0u8; (w * h * 4) as usize];
            let mut ok = blit.is_ok();
            if ok {
                let lines = GetDIBits(
                    hdc_mem,
                    hbmp,
                    0,
                    h,
                    Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
                    &mut bi,
                    DIB_RGB_COLORS,
                );
                ok = lines == h as i32;
            }
            SelectObject(hdc_mem, old);
            let _ = DeleteObject(hbmp);
            let _ = DeleteDC(hdc_mem);
            let _ = DeleteDC(hdc_screen);
            if !ok {
                return Err("GDI BitBlt/GetDIBits failed".into());
            }
            Ok(Frame {
                bgra: buf,
                w,
                h,
                origin: (rect.left, rect.top),
            })
        }
    }

    fn ensure_dup(&mut self, ai: usize, oi: usize) -> Result<(), String> {
        let ok = self
            .dup
            .as_ref()
            .map(|d| d.key == (ai, oi))
            .unwrap_or(false);
        if !ok {
            self.create_dup(ai, oi)?;
        }
        Ok(())
    }

    fn grab(&mut self) -> Result<Frame, String> {
        let dup_state = self
            .dup
            .as_mut()
            .ok_or_else(|| "duplication not initialized".to_string())?;
        unsafe {
            // First acquire after (re)creation returns the full desktop.
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource_opt: Option<windows::Win32::Graphics::Dxgi::IDXGIResource> = None;
            if let Err(e) =
                dup_state
                    .duplication
                    .AcquireNextFrame(200, &mut frame_info, &mut resource_opt)
            {
                // Classify by HRESULT code, not message text (Windows
                // localizes errors, e.g. the Russian DXGI timeout message
                // "Время ожидания истекло" contains no English "timeout").
                match e.code().0 as u32 {
                    0x887A_0027 => {
                        // DXGI_ERROR_WAIT_TIMEOUT: screen did not change.
                        return Err("frame timeout (screen did not update)".into());
                    }
                    0x887A_0026 => {
                        // DXGI_ERROR_ACCESS_LOST.
                        return Err("desktop duplication access lost".into());
                    }
                    _ => return Err(format!("AcquireNextFrame failed: {e}")),
                }
            }
            let resource =
                resource_opt.ok_or("AcquireNextFrame returned no desktop resource")?;
            let tex: ID3D11Texture2D = resource
                .cast::<ID3D11Texture2D>()
                .map_err(|e| format!("QI(ID3D11Texture2D) failed: {e}"))?;
            let dst: ID3D11Resource = dup_state
                .staging
                .cast::<ID3D11Resource>()
                .map_err(|e| format!("QI(staging resource) failed: {e}"))?;
            let src: ID3D11Resource = tex
                .cast::<ID3D11Resource>()
                .map_err(|e| format!("QI(frame resource) failed: {e}"))?;
            dup_state.context.CopyResource(&dst, &src);
            let _ = dup_state.duplication.ReleaseFrame();

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            dup_state
                .context
                .Map(&dst, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| format!("Map(staging) failed: {e}"))?;
            let (w, h) = (dup_state.w as usize, dup_state.h as usize);
            let mut bgra = Vec::with_capacity(w * h * 4);
            let base = mapped.pData as *const u8;
            let pitch = mapped.RowPitch as usize;
            for row in 0..h {
                let line = std::slice::from_raw_parts(base.add(row * pitch), w * 4);
                bgra.extend_from_slice(line);
            }
            dup_state.context.Unmap(&dst, 0);
            let origin = dup_state.origin;
            Ok(Frame {
                bgra,
                w: w as u32,
                h: h as u32,
                origin,
            })
        }
    }
}

// ---- pure helpers (unit-testable, no COM) ----

/// Crop a packed BGRA buffer (stride == w*4) to rect, clamped to bounds.
/// Returns the new packed buffer and dimensions.
pub fn crop_bgra(
    src: &[u8],
    w: u32,
    h: u32,
    x: i32,
    y: i32,
    cw: u32,
    ch: u32,
) -> (Vec<u8>, u32, u32) {
    let x0 = x.clamp(0, w as i32) as u32;
    let y0 = y.clamp(0, h as i32) as u32;
    let cw = cw.min(w.saturating_sub(x0));
    let ch = ch.min(h.saturating_sub(y0));
    let mut out = Vec::with_capacity((cw * ch * 4) as usize);
    for row in y0..(y0 + ch) {
        let start = ((row * w + x0) * 4) as usize;
        let end = start + (cw * 4) as usize;
        if end <= src.len() {
            out.extend_from_slice(&src[start..end]);
        }
    }
    (out, cw, ch)
}

/// Convert packed BGRA into an RGB image buffer for the `image` crate.
pub fn bgra_to_rgb(src: &[u8], w: u32, h: u32) -> image::RgbImage {
    let mut img = image::RgbImage::new(w, h);
    for (i, px) in img.pixels_mut().enumerate() {
        let o = i * 4;
        if o + 2 < src.len() {
            *px = image::Rgb([src[o + 2], src[o + 1], src[o]]);
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2x2 BGRA: B=0,G=1,R=2,A=3 per pixel.
    fn sample() -> (Vec<u8>, u32, u32) {
        (
            vec![
                10, 20, 30, 255, 40, 50, 60, 255, // row 0
                70, 80, 90, 255, 100, 110, 120, 255, // row 1
            ],
            2,
            2,
        )
    }

    #[test]
    fn crop_full() {
        let (src, w, h) = sample();
        let (out, cw, ch) = crop_bgra(&src, w, h, 0, 0, 2, 2);
        assert_eq!((cw, ch, out.len()), (2, 2, 16));
        assert_eq!(out, src);
    }

    #[test]
    fn crop_partial() {
        let (src, w, h) = sample();
        let (out, cw, ch) = crop_bgra(&src, w, h, 1, 1, 5, 5);
        assert_eq!((cw, ch), (1, 1));
        assert_eq!(out, vec![100, 110, 120, 255]);
    }

    #[test]
    fn crop_clamps_out_of_bounds() {
        let (src, w, h) = sample();
        let (out, cw, ch) = crop_bgra(&src, w, h, -10, -10, 2, 2);
        assert_eq!((cw, ch), (2, 2));
        assert_eq!(out, src);
        let (_, cw, ch) = crop_bgra(&src, w, h, 5, 5, 2, 2);
        assert_eq!((cw, ch), (0, 0));
    }

    #[test]
    fn bgra_to_rgb_swaps_channels() {
        let (src, w, h) = sample();
        let img = bgra_to_rgb(&src, w, h);
        assert_eq!(img.get_pixel(0, 0).0, [30, 20, 10]);
        assert_eq!(img.get_pixel(1, 1).0, [120, 110, 100]);
    }
}
