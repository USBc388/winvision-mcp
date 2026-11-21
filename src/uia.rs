//! UI Automation engine.
//!
//! `desktop_snapshot` walks the UIA tree using a batched CacheRequest
//! (TreeScope=Subtree) so all properties for the whole window arrive in a
//! single cross-process round trip — typically < 20 ms. Elements get short
//! IDs [1..N]; the live IUIAutomationElement handles are cached for 10 s so
//! `ui_interact` can act on them instantly.
//!
//! `ui_interact` first tries native UIA patterns (Invoke / Value / Toggle /
//! SelectionItem / ExpandCollapse) which work even when the element is not
//! hovered; if the element is custom-drawn it falls back to a hardware
//! SendInput click on the element's center.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use windows::core::{w, BSTR, Interface};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, SendMessageTimeoutW, SystemParametersInfoW, SMTO_ABORTIFHUNG, SPIF_SENDCHANGE,
    SPI_GETSCREENREADER, SPI_SETSCREENREADER, WM_GETOBJECT,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, IUIAutomationCondition,
    IUIAutomationElement, IUIAutomationElementArray, IUIAutomationExpandCollapsePattern,
    IUIAutomationInvokePattern, IUIAutomationSelectionItemPattern, IUIAutomationTogglePattern,
    IUIAutomationValuePattern, TreeScope_Subtree, UIA_AutomationIdPropertyId,
    UIA_BoundingRectanglePropertyId, UIA_ClassNamePropertyId, UIA_ControlTypePropertyId,
    UIA_ExpandCollapsePatternId, UIA_InvokePatternId, UIA_IsEnabledPropertyId,
    UIA_IsExpandCollapsePatternAvailablePropertyId, UIA_IsInvokePatternAvailablePropertyId,
    UIA_IsKeyboardFocusablePropertyId, UIA_IsOffscreenPropertyId,
    UIA_IsSelectionItemPatternAvailablePropertyId, UIA_IsTogglePatternAvailablePropertyId,
    UIA_IsValuePatternAvailablePropertyId, UIA_NamePropertyId, UIA_SelectionItemPatternId,
    UIA_TogglePatternId, UIA_ValuePatternId, UIA_ValueValuePropertyId,
};

use crate::window::{self, WindowInfo};

pub const CACHE_TTL_MS: u128 = 10_000;

/// Processes that render web pages with an opt-in accessibility tree.
/// Chromium builds its UIA/IA2 tree lazily; Gecko (Firefox / Zen / Floorp /
/// LibreWolf) likewise — both wake instantly on WM_GETOBJECT + screen-reader
/// mode (see `wake_browser_a11y`).
pub const BROWSER_PROCESSES: &[&str] = &[
    "chrome.exe",
    "msedge.exe",
    "zen.exe",
    "firefox.exe",
    "brave.exe",
    "floorp.exe",
    "librewolf.exe",
    "waterfox.exe",
    "opera.exe",
    "vivaldi.exe",
    "thorium.exe",
    "chromium.exe",
    "whale.exe",
];

pub fn is_browser_process(proc_name: &str) -> bool {
    BROWSER_PROCESSES
        .iter()
        .any(|b| proc_name.trim().eq_ignore_ascii_case(b))
}

/// Web pages nest deeply (div soup); give browsers extra depth when the
/// caller did not request more.
pub fn effective_depth(requested: u32, browser: bool) -> u32 {
    if browser {
        requested.clamp(14, 15)
    } else {
        requested
    }
}

fn get_screen_reader_flag() -> BOOL {
    unsafe {
        let mut v = BOOL(0);
        let _ = SystemParametersInfoW(
            SPI_GETSCREENREADER,
            0,
            Some(&mut v as *mut BOOL as *mut std::ffi::c_void),
            windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        v
    }
}

fn set_screen_reader_flag(on: bool) {
    unsafe {
        let val = BOOL(if on { 1 } else { 0 });
        // SPIF_SENDCHANGE broadcasts WM_SETTINGCHANGE so running browsers
        // notice immediately without a restart.
        let _ = SystemParametersInfoW(
            SPI_SETSCREENREADER,
            0,
            Some(&val as *const BOOL as *const std::ffi::c_void as *mut std::ffi::c_void),
            SPIF_SENDCHANGE,
        );
    }
}

pub struct UiaCtx {
    automation: IUIAutomation,
    cache: HashMap<u32, IUIAutomationElement>,
    rects: HashMap<u32, (i32, i32, i32, i32)>,
    cache_at: Option<Instant>,
}

#[derive(Default)]
struct ElInfo {
    name: String,
    ctype: i32,
    rect: Option<(i32, i32, i32, i32)>,
    offscreen: bool,
    enabled: bool,
    focusable: bool,
    value: Option<String>,
    invoke: bool,
    toggle: bool,
    value_avail: bool,
    expand: bool,
    select_item: bool,
}

impl UiaCtx {
    pub fn new() -> Result<Self, String> {
        unsafe {
            let automation: IUIAutomation = windows::Win32::System::Com::CoCreateInstance(
                &CUIAutomation,
                None,
                CLSCTX_INPROC_SERVER,
            )
            .map_err(|e| format!("CoCreateInstance(CUIAutomation) failed: {e}"))?;
            Ok(Self {
                automation,
                cache: HashMap::new(),
                rects: HashMap::new(),
                cache_at: None,
            })
        }
    }

    fn resolve_root(
        &self,
        target: Option<&str>,
    ) -> Result<(IUIAutomationElement, WindowInfo), String> {
        let info = match target {
            None => {
                let fg = window::foreground_hwnd();
                if fg == 0 {
                    return Err("no foreground window; pass target_window".into());
                }
                window::window_info(HWND(fg as *mut _))
                    .ok_or_else(|| "foreground window vanished".to_string())?
            }
            Some(q) => window::find_window(q).ok_or_else(|| {
                let mut names: Vec<String> = window::collect_windows(false)
                    .iter()
                    .map(|w| format!("\"{}\" ({})", w.title, w.process))
                    .take(12)
                    .collect();
                names.sort();
                format!(
                    "no window matches '{q}'. Top-level windows:\n  {}",
                    names.join("\n  ")
                )
            })?,
        };
        unsafe {
            let el = self
                .automation
                .ElementFromHandle(HWND(info.hwnd as *mut _))
                .map_err(|e| format!("ElementFromHandle failed for \"{}\": {e}", info.title))?;
            Ok((el, info))
        }
    }

    fn build_cache_request(&self, minimal: bool) -> Result<IUIAutomationCacheRequest, String> {
        unsafe {
            let req = self
                .automation
                .CreateCacheRequest()
                .map_err(|e| format!("CreateCacheRequest failed: {e}"))?;
            let props: &[windows::Win32::UI::Accessibility::UIA_PROPERTY_ID] = if minimal {
                &[
                    UIA_NamePropertyId,
                    UIA_ControlTypePropertyId,
                    UIA_BoundingRectanglePropertyId,
                    UIA_IsOffscreenPropertyId,
                    UIA_IsEnabledPropertyId,
                    UIA_IsInvokePatternAvailablePropertyId,
                    UIA_IsValuePatternAvailablePropertyId,
                ]
            } else {
                &[
                    UIA_NamePropertyId,
                    UIA_ControlTypePropertyId,
                    UIA_BoundingRectanglePropertyId,
                    UIA_IsOffscreenPropertyId,
                    UIA_IsEnabledPropertyId,
                    UIA_IsKeyboardFocusablePropertyId,
                    UIA_AutomationIdPropertyId,
                    UIA_ClassNamePropertyId,
                    UIA_ValueValuePropertyId,
                    UIA_IsInvokePatternAvailablePropertyId,
                    UIA_IsValuePatternAvailablePropertyId,
                    UIA_IsTogglePatternAvailablePropertyId,
                    UIA_IsExpandCollapsePatternAvailablePropertyId,
                    UIA_IsSelectionItemPatternAvailablePropertyId,
                ]
            };
            for p in props {
                req.AddProperty(*p)
                    .map_err(|e| format!("AddProperty failed: {e}"))?;
            }
            req.SetTreeScope(TreeScope_Subtree)
                .map_err(|e| format!("SetTreeScope failed: {e}"))?;
            let filter: IUIAutomationCondition = self
                .automation
                .ControlViewCondition()
                .map_err(|e| format!("ControlViewCondition failed: {e}"))?;
            req.SetTreeFilter(&filter)
                .map_err(|e| format!("SetTreeFilter failed: {e}"))?;
            Ok(req)
        }
    }

    /// Wake a browser's accessibility tree: announce screen-reader mode and
    /// poke the window (and Chromium's renderer child) with WM_GETOBJECT /
    /// OBJID_CLIENT — the exact probe screen readers make. Chromium and
    /// Gecko build their full DOM/UIA tree on demand only. Returns the
    /// previous screen-reader flag if we flipped it (caller restores).
    fn wake_browser_a11y(&self, hwnd: isize) -> Option<BOOL> {
        let prev = get_screen_reader_flag();
        if prev.0 == 0 {
            set_screen_reader_flag(true);
        }
        unsafe {
            let targets: Vec<HWND> = {
                let top = HWND(hwnd as *mut _);
                let mut v = vec![top];
                // Chromium exposes the page through this child hwnd.
                if let Ok(renderer) = FindWindowExW(
                    top,
                    HWND::default(),
                    w!("Chrome_RenderWidgetHostHWND"),
                    None,
                ) {
                    if !renderer.0.is_null() {
                        v.push(renderer);
                    }
                }
                v
            };
            for h in targets {
                let mut result: usize = 0;
                let _ = SendMessageTimeoutW(
                    h,
                    WM_GETOBJECT,
                    WPARAM(0),
                    LPARAM(windows::Win32::UI::WindowsAndMessaging::OBJID_CLIENT.0 as isize),
                    SMTO_ABORTIFHUNG,
                    1000,
                    Some(&mut result),
                );
            }
        }
        // Give the renderer time to build the tree (Chromium needs ~100-250 ms
        // for a typical page; Gecko is faster).
        std::thread::sleep(Duration::from_millis(250));
        if prev.0 == 0 {
            Some(prev)
        } else {
            None
        }
    }

    /// Snapshot one window's UIA tree into a compact ID-tagged listing.
    pub fn snapshot(
        &mut self,
        target: Option<&str>,
        max_depth: u32,
        max_elements: u32,
    ) -> Result<String, String> {
        let (root_el, info) = self.resolve_root(target)?;

        // Browsers render pages with an opt-in accessibility tree — wake it
        // first and give web pages extra walk depth.
        let browser = is_browser_process(&info.process);
        let prev_screen_reader = if browser {
            self.wake_browser_a11y(info.hwnd)
        } else {
            None
        };

        let walk_result = self.walk_snapshot(&root_el, &info, max_depth, max_elements, browser);

        if let Some(prev) = prev_screen_reader {
            set_screen_reader_flag(prev.0 != 0);
        }
        walk_result
    }

    fn walk_snapshot(
        &mut self,
        root_el: &IUIAutomationElement,
        info: &WindowInfo,
        max_depth: u32,
        max_elements: u32,
        browser: bool,
    ) -> Result<String, String> {
        let started = Instant::now();
        // Batched subtree fetch; fall back to a minimal property set for
        // providers that reject pattern-availability properties.
        let cached_root = {
            let req = match self.build_cache_request(false) {
                Ok(r) => r,
                Err(_) => self.build_cache_request(true)?,
            };
            unsafe {
                root_el
                    .BuildUpdatedCache(&req)
                    .map_err(|e| format!("BuildUpdatedCache failed: {e}"))?
            }
        };

        let mut st = WalkState {
            out: String::with_capacity(4096),
            map: HashMap::new(),
            rects: HashMap::new(),
            count: 0,
            max_elements: max_elements.clamp(1, 1000),
            depth_limit: effective_depth(max_depth, browser),
        };
        walk(&cached_root, 0, &mut st);

        self.cache = std::mem::take(&mut st.map);
        self.rects = std::mem::take(&mut st.rects);
        self.cache_at = Some(Instant::now());

        let ms = started.elapsed().as_millis();
        Ok(format!(
            "Snapshot of \"{}\" ({}, hwnd 0x{:x}, pid {})\n{} elements, {} ms, IDs valid for {} s:\n\n{}\n(hint: act on elements with ui_interact element_id=[N])",
            info.title,
            info.process,
            info.hwnd,
            info.pid,
            self.cache.len(),
            ms,
            CACHE_TTL_MS / 1000,
            st.out
        ))
    }

    fn check_ttl(&self) -> Result<(), String> {
        match self.cache_at {
            Some(t) if t.elapsed().as_millis() <= CACHE_TTL_MS => Ok(()),
            Some(t) => Err(format!(
                "element cache expired ({ms} ms old > 10 s TTL) — run desktop_snapshot again",
                ms = t.elapsed().as_millis()
            )),
            None => Err("no snapshot yet — run desktop_snapshot first".into()),
        }
    }

    /// Perform an action on a cached element. Returns a description.
    pub fn interact(
        &mut self,
        element_id: u32,
        action: &str,
        text: Option<&str>,
    ) -> Result<String, String> {
        self.check_ttl()?;
        let el = self
            .cache
            .get(&element_id)
            .ok_or_else(|| {
                format!(
                    "element_id {element_id} not in cache (valid 1..={}) — run desktop_snapshot again",
                    self.cache.len().max(1)
                )
            })?
            .clone();

        let desc = match action {
            "focus" => unsafe {
                el.SetFocus().map_err(|e| format!("SetFocus failed: {e}"))?;
                "focused element via UIA".to_string()
            },
            "set_value" => {
                let text = text.ok_or_else(|| "set_value requires 'text' parameter".to_string())?;
                if let Some(d) = try_set_value(&el, text) {
                    d
                } else {
                    // Fallback: focus, select all, retype.
                    unsafe {
                        el.SetFocus().map_err(|e| format!("SetFocus failed: {e}"))?;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    crate::input::send_hotkey("Ctrl+a")?;
                    std::thread::sleep(std::time::Duration::from_millis(40));
                    crate::input::type_text(text)?;
                    format!(
                        "typed {len} chars via SendInput (no UIA ValuePattern on this element)",
                        len = text.chars().count()
                    )
                }
            }
            "toggle" => {
                if let Some(d) = try_toggle(&el) {
                    d
                } else {
                    hardware_click_on(&el, 1)?
                }
            }
            "click" => {
                if let Some(d) = try_invoke(&el) {
                    d
                } else if let Some(d) = try_select(&el) {
                    d
                } else if let Some(d) = try_expand(&el) {
                    d
                } else {
                    hardware_click_on(&el, 1)?
                }
            }
            "double_click" => hardware_click_on(&el, 2)?,
            "right_click" => hardware_click_on(&el, -1)?,
            _ => {
                return Err(format!(
                    "unknown action '{action}' (use click|double_click|right_click|focus|set_value|toggle)"
                ))
            }
        };
        Ok(desc)
    }
}

struct WalkState {
    out: String,
    map: HashMap<u32, IUIAutomationElement>,
    rects: HashMap<u32, (i32, i32, i32, i32)>,
    count: u32,
    max_elements: u32,
    depth_limit: u32,
}

/// Read all cached properties off one element (no cross-process calls).
fn read_info(el: &IUIAutomationElement) -> ElInfo {
    unsafe {
        let s = |r: Result<windows::core::BSTR, _>| {
            r.map(|h| h.to_string())
                .unwrap_or_default()
        };
        let b = |r: Result<BOOL, _>| r.map(|v| v.0 != 0).unwrap_or(false);
        let rect_of = |r: Result<RECT, _>| {
            r.ok().map(|r| {
                (
                    r.left,
                    r.top,
                    (r.right - r.left).max(0),
                    (r.bottom - r.top).max(0),
                )
            })
        };
        let avail =
            |v: Result<windows::core::VARIANT, _>| v.ok().and_then(|v| bool::try_from(&v).ok()).unwrap_or(false);
        ElInfo {
            name: s(el.CachedName()),
            ctype: el.CachedControlType().map(|t| t.0).unwrap_or(0),
            rect: rect_of(el.CachedBoundingRectangle()),
            offscreen: b(el.CachedIsOffscreen()),
            enabled: b(el.CachedIsEnabled()),
            focusable: b(el.CachedIsKeyboardFocusable()),
            value: el
                .GetCachedPropertyValue(UIA_ValueValuePropertyId)
                .ok()
                .and_then(|v| BSTR::try_from(&v).ok().map(|b| b.to_string())),
            invoke: avail(el.GetCachedPropertyValue(UIA_IsInvokePatternAvailablePropertyId)),
            toggle: avail(el.GetCachedPropertyValue(UIA_IsTogglePatternAvailablePropertyId)),
            value_avail: avail(el.GetCachedPropertyValue(UIA_IsValuePatternAvailablePropertyId)),
            expand: avail(el.GetCachedPropertyValue(UIA_IsExpandCollapsePatternAvailablePropertyId)),
            select_item: avail(el.GetCachedPropertyValue(
                UIA_IsSelectionItemPatternAvailablePropertyId,
            )),
        }
    }
}

/// Type name for a UIA control type id, compared against crate constants.
fn ctype_name(t: i32) -> &'static str {
    use windows::Win32::UI::Accessibility::*;
    let m: &[(i32, &str)] = &[
        (UIA_ButtonControlTypeId.0, "Button"),
        (UIA_CheckBoxControlTypeId.0, "CheckBox"),
        (UIA_ComboBoxControlTypeId.0, "ComboBox"),
        (UIA_EditControlTypeId.0, "Edit"),
        (UIA_HyperlinkControlTypeId.0, "Hyperlink"),
        (UIA_ImageControlTypeId.0, "Image"),
        (UIA_ListItemControlTypeId.0, "ListItem"),
        (UIA_ListControlTypeId.0, "List"),
        (UIA_MenuControlTypeId.0, "Menu"),
        (UIA_MenuBarControlTypeId.0, "MenuBar"),
        (UIA_MenuItemControlTypeId.0, "MenuItem"),
        (UIA_ProgressBarControlTypeId.0, "ProgressBar"),
        (UIA_RadioButtonControlTypeId.0, "RadioButton"),
        (UIA_ScrollBarControlTypeId.0, "ScrollBar"),
        (UIA_SliderControlTypeId.0, "Slider"),
        (UIA_SpinnerControlTypeId.0, "Spinner"),
        (UIA_StatusBarControlTypeId.0, "StatusBar"),
        (UIA_TabControlTypeId.0, "Tab"),
        (UIA_TabItemControlTypeId.0, "TabItem"),
        (UIA_TableControlTypeId.0, "Table"),
        (UIA_TextControlTypeId.0, "Text"),
        (UIA_ThumbControlTypeId.0, "Thumb"),
        (UIA_TitleBarControlTypeId.0, "TitleBar"),
        (UIA_ToolBarControlTypeId.0, "ToolBar"),
        (UIA_ToolTipControlTypeId.0, "ToolTip"),
        (UIA_TreeControlTypeId.0, "Tree"),
        (UIA_TreeItemControlTypeId.0, "TreeItem"),
        (UIA_DocumentControlTypeId.0, "Document"),
        (UIA_PaneControlTypeId.0, "Pane"),
        (UIA_WindowControlTypeId.0, "Window"),
        (UIA_HeaderControlTypeId.0, "Header"),
        (UIA_HeaderItemControlTypeId.0, "HeaderItem"),
        (UIA_GroupControlTypeId.0, "Group"),
        (UIA_DataGridControlTypeId.0, "DataGrid"),
        (UIA_DataItemControlTypeId.0, "DataItem"),
        (UIA_CustomControlTypeId.0, "Custom"),
    ];
    m.iter()
        .find(|(id, _)| *id == t)
        .map(|(_, n)| *n)
        .unwrap_or("Custom")
}

/// Decides whether a node deserves a line in the snapshot output.
/// Unnamed pure containers (Pane/Group/Custom) collapse into their children;
/// dead leaves are dropped; interactive nodes always survive.
fn keep_node(i: &ElInfo, is_root: bool) -> bool {
    if is_root {
        return true;
    }
    if is_interactive(i) {
        return true;
    }
    // Hidden empty wrappers (Gecko emits dozens of unnamed offscreen Menus).
    if i.offscreen
        && i.name.trim().is_empty()
        && i.rect.map(|(_, _, w, h)| w == 0 && h == 0).unwrap_or(true)
    {
        return false;
    }
    let unnamed_container = i.name.trim().is_empty()
        && matches!(
            ctype_name(i.ctype),
            "Pane" | "Group" | "Custom" | "Window" | "Table"
        );
    if unnamed_container {
        return false;
    }
    if i.rect
        .map(|(_, _, w, h)| w <= 0 && h <= 0)
        .unwrap_or(true)
        && i.name.trim().is_empty()
    {
        return false;
    }
    if matches!(
        ctype_name(i.ctype),
        "Text" | "Image" | "Header" | "HeaderItem" | "Thumb"
    ) && i.name.trim().is_empty()
        && i.value.is_none()
    {
        return false;
    }
    !i.name.trim().is_empty() || i.value.is_some()
}

fn is_interactive(i: &ElInfo) -> bool {
    i.invoke || i.toggle || i.value_avail || i.expand || i.select_item || i.focusable
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

fn fmt_line(id: u32, depth: u32, i: &ElInfo) -> String {
    let indent = "  ".repeat(depth as usize);
    let mut s = format!(
        "{}[{}] {} \"{}\"",
        indent,
        id,
        ctype_name(i.ctype),
        trunc(&i.name, 80)
    );
    if let Some(v) = &i.value {
        if !v.is_empty() {
            s.push_str(&format!(" value=\"{}\"", trunc(v, 60)));
        }
    }
    if !i.enabled {
        s.push_str(" (disabled)");
    } else if i.offscreen {
        s.push_str(" (offscreen)");
    }
    if let Some((x, y, w, h)) = i.rect {
        s.push_str(&format!(" (x: {x}, y: {y}, w: {w}, h: {h})"));
    }
    s
}

/// Recursive walk of the *cached* subtree (no cross-process calls per node).
fn walk(el: &IUIAutomationElement, depth: u32, st: &mut WalkState) {
    let is_root = depth == 0;
    let info = read_info(el);
    let emit = keep_node(&info, is_root);
    if emit && st.count < st.max_elements {
        st.count += 1;
        let id = st.count;
        st.out.push_str(&fmt_line(id, depth, &info));
        st.out.push('\n');
        if let Some(r) = info.rect {
            st.rects.insert(id, r);
        }
        st.map.insert(id, el.clone());
    }
    if depth >= st.depth_limit || st.count >= st.max_elements {
        return;
    }
    // Cached children: already fetched with the subtree cache request.
    let children: IUIAutomationElementArray = unsafe {
        match el.GetCachedChildren() {
            Ok(c) => c,
            Err(_) => return,
        }
    };
    let n = unsafe { children.Length().unwrap_or(0) } as u32;
    for i in 0..n {
        let child = unsafe {
            match children.GetElement(i as i32) {
                Ok(c) => c,
                Err(_) => break,
            }
        };
        walk(&child, depth + 1, st);
        if st.count >= st.max_elements {
            break;
        }
    }
}

// ---- pattern helpers (used by ui_interact) ----

fn get_pattern<T: Interface>(
    el: &IUIAutomationElement,
    pid: windows::Win32::UI::Accessibility::UIA_PATTERN_ID,
) -> Option<T> {
    unsafe {
        let unk = el.GetCurrentPattern(pid).ok()?;
        unk.cast::<T>().ok()
    }
}

fn try_invoke(el: &IUIAutomationElement) -> Option<String> {
    let p: IUIAutomationInvokePattern = get_pattern(el, UIA_InvokePatternId)?;
    unsafe { p.Invoke().ok()? };
    Some("clicked via UIA InvokePattern (no mouse movement needed)".into())
}

fn try_select(el: &IUIAutomationElement) -> Option<String> {
    let p: IUIAutomationSelectionItemPattern = get_pattern(el, UIA_SelectionItemPatternId)?;
    unsafe { p.Select().ok()? };
    Some("selected via UIA SelectionItemPattern".into())
}

fn try_expand(el: &IUIAutomationElement) -> Option<String> {
    let p: IUIAutomationExpandCollapsePattern = get_pattern(el, UIA_ExpandCollapsePatternId)?;
    unsafe { p.Expand().ok()? };
    Some("expanded via UIA ExpandCollapsePattern".into())
}

fn try_toggle(el: &IUIAutomationElement) -> Option<String> {
    let p: IUIAutomationTogglePattern = get_pattern(el, UIA_TogglePatternId)?;
    unsafe { p.Toggle().ok()? };
    Some("toggled via UIA TogglePattern".into())
}

fn try_set_value(el: &IUIAutomationElement, text: &str) -> Option<String> {
    let p: IUIAutomationValuePattern = get_pattern(el, UIA_ValuePatternId)?;
    unsafe {
        let bstr = windows::core::BSTR::from(text);
        p.SetValue(&bstr).ok()?;
    }
    Some(format!(
        "set value via UIA ValuePattern ({len} chars)",
        len = text.chars().count()
    ))
}

fn hardware_click_on(el: &IUIAutomationElement, button_clicks: i32) -> Result<String, String> {
    unsafe {
        let r = el.CurrentBoundingRectangle().map_err(|e| {
            format!("CurrentBoundingRectangle failed: {e} (element may be gone — re-snapshot)")
        })?;
        // Focus the owning window first so the click lands in the right place.
        if let Ok(hwnd) = el.CurrentNativeWindowHandle() {
            if hwnd.0 as isize != window::foreground_hwnd() {
                let _ = window::focus_window(hwnd.0 as isize);
            }
        }
        let w = (r.right - r.left).max(0);
        let h = (r.bottom - r.top).max(0);
        if w <= 0 || h <= 0 {
            return Err("element has an empty bounding rect; cannot hardware-click".into());
        }
        let cx = r.left + w / 2;
        let cy = r.top + h / 2;
        let (button, clicks) = if button_clicks < 0 {
            ("right", 1)
        } else {
            ("left", button_clicks.max(1) as u32)
        };
        crate::input::click_at(cx, cy, button, clicks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(
        name: &str,
        ctype: i32,
        rect: Option<(i32, i32, i32, i32)>,
        interactive: bool,
    ) -> ElInfo {
        ElInfo {
            name: name.to_string(),
            ctype,
            rect,
            offscreen: false,
            enabled: true,
            focusable: false,
            value: None,
            invoke: interactive,
            toggle: false,
            value_avail: false,
            expand: false,
            select_item: false,
        }
    }

    fn ctype_id(name: &str) -> i32 {
        use windows::Win32::UI::Accessibility::*;
        match name {
            "Button" => UIA_ButtonControlTypeId.0,
            "Edit" => UIA_EditControlTypeId.0,
            "Pane" => UIA_PaneControlTypeId.0,
            "Text" => UIA_TextControlTypeId.0,
            "Image" => UIA_ImageControlTypeId.0,
            "Window" => UIA_WindowControlTypeId.0,
            "Table" => UIA_TableControlTypeId.0,
            _ => 0,
        }
    }

    #[test]
    fn keeps_interactive_even_offscreen() {
        let mut i = info("btn", ctype_id("Button"), Some((0, 0, 10, 10)), true);
        i.offscreen = true;
        assert!(keep_node(&i, false));
    }

    #[test]
    fn drops_unnamed_panes() {
        let i = info("", ctype_id("Pane"), Some((0, 0, 100, 100)), false);
        assert!(!keep_node(&i, false));
    }

    #[test]
    fn keeps_named_containers() {
        let i = info("Sidebar", ctype_id("Pane"), Some((0, 0, 100, 100)), false);
        assert!(keep_node(&i, false));
    }

    #[test]
    fn drops_dead_text_leaves() {
        let i = info("", ctype_id("Text"), Some((0, 0, 50, 10)), false);
        assert!(!keep_node(&i, false));
        let mut v = info("", ctype_id("Text"), Some((0, 0, 50, 10)), false);
        v.value = Some("dynamic text".into());
        assert!(keep_node(&v, false));
    }

    #[test]
    fn truncates_long_names() {
        let s = trunc("abcdefghij", 5);
        assert!(s.chars().count() == 6 && s.ends_with('…'));
    }

    #[test]
    fn control_type_names_resolve() {
        assert_eq!(ctype_name(ctype_id("Button")), "Button");
        assert_eq!(ctype_name(ctype_id("Edit")), "Edit");
        assert_eq!(ctype_name(0), "Custom");
        assert_eq!(ctype_name(0x7F00_0000), "Custom");
    }

    #[test]
    fn fmt_line_matches_spec_shape() {
        let i = info("Run Terminal", ctype_id("Button"), Some((1420, 84, 32, 32)), true);
        let line = fmt_line(2, 1, &i);
        assert!(line.contains("[2] Button \"Run Terminal\" (x: 1420, y: 84, w: 32, h: 32)"));
    }

    #[test]
    fn browser_detection() {
        for b in ["chrome.exe", "Chrome.EXE", "msedge.exe", "zen.exe", "firefox.exe",
                  "brave.exe", "floorp.exe", "librewolf.exe"] {
            assert!(is_browser_process(b), "must detect {b}");
        }
        assert!(is_browser_process(" floorp.exe "), "trims whitespace");
        for n in ["notepad.exe", "explorer.exe", "mspaint.exe", ""] {
            assert!(!is_browser_process(n), "must not detect {n}");
        }
    }

    #[test]
    fn browser_depth_bumped_to_14() {
        assert_eq!(effective_depth(6, true), 14);
        assert_eq!(effective_depth(12, true), 14);
        assert_eq!(effective_depth(15, true), 15);
        assert_eq!(effective_depth(6, false), 6);
        assert_eq!(effective_depth(3, false), 3);
    }
}
