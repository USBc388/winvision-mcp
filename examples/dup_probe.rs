//! Diagnostic: try several Desktop Duplication creation strategies to find
//! one that works on this machine. Run: cargo run --example dup_probe

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_FLAG,
    D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication,
};

fn try_variant(name: &str, make: impl FnOnce() -> Result<IDXGIOutputDuplication, String>) {
    match make() {
        Ok(_) => println!("OK   — {name}"),
        Err(e) => println!("FAIL — {name}: {e}"),
    }
}

fn levels() -> [windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL; 2] {
    [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]
}

fn main() {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1::<IDXGIFactory1>().unwrap();
        let adapter = factory.EnumAdapters1(0).unwrap();
        let output = adapter.EnumOutputs(0).expect("no outputs on adapter 0");
        let output1: IDXGIOutput1 = output.cast::<IDXGIOutput1>().unwrap();
        let adapter_base: windows::Win32::Graphics::Dxgi::IDXGIAdapter =
            adapter.cast::<windows::Win32::Graphics::Dxgi::IDXGIAdapter>().unwrap();

        let make_device = |flags: D3D11_CREATE_DEVICE_FLAG,
                               use_adapter: Option<&windows::Win32::Graphics::Dxgi::IDXGIAdapter>,
                               driver: windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE|
         -> Result<ID3D11Device, String> {
            let mut dev: Option<ID3D11Device> = None;
            let mut fl = D3D_FEATURE_LEVEL_11_0;
            let mut ctx: Option<windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext> = None;
            let l = levels();
            D3D11CreateDevice(
                use_adapter,
                driver,
                HMODULE::default(),
                flags,
                Some(&l),
                D3D11_SDK_VERSION,
                Some(&mut dev),
                Some(&mut fl),
                Some(&mut ctx),
            )
            .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
            dev.ok_or_else(|| "no device".into())
        };

        try_variant("device(adapter0, flags=0), dup on output0", || {
            let d = make_device(D3D11_CREATE_DEVICE_FLAG(0), Some(&adapter_base), D3D_DRIVER_TYPE_UNKNOWN)?;
            output1.DuplicateOutput(&d).map_err(|e| e.to_string())
        });

        try_variant("device(adapter0, BGRA), dup on output0", || {
            let d = make_device(D3D11_CREATE_DEVICE_BGRA_SUPPORT, Some(&adapter_base), D3D_DRIVER_TYPE_UNKNOWN)?;
            output1.DuplicateOutput(&d).map_err(|e| e.to_string())
        });

        try_variant("device(HARDWARE default), dup on output0", || {
            let d = make_device(D3D11_CREATE_DEVICE_FLAG(0), None, D3D_DRIVER_TYPE_HARDWARE)?;
            output1.DuplicateOutput(&d).map_err(|e| e.to_string())
        });

        try_variant("device(HARDWARE default, BGRA), dup on output0", || {
            let d = make_device(D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D_DRIVER_TYPE_HARDWARE)?;
            output1.DuplicateOutput(&d).map_err(|e| e.to_string())
        });

        // Report output info.
        let desc = output.GetDesc().unwrap();
        println!("output: {:?}", String::from_utf16_lossy(&desc.DeviceName));
        println!("desktop coords: {:?}", desc.DesktopCoordinates);
        println!("attached: {}", desc.AttachedToDesktop.0 != 0);
    }
}
