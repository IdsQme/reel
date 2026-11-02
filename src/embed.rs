//! Shows mpv's video inside the terminal window, over a rectangle of cells.
//! On Windows this is a real child window of the terminal, handed to mpv with --wid.
//! Where that is not possible `VideoHost::create` returns None and mpv gets its own window.
use ratatui::layout::Rect;

#[cfg(windows)]
mod imp {
    use super::Rect;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::sync::mpsc::channel;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{BLACK_BRUSH, GetMonitorInfoW, GetStockObject, HBRUSH, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetAsyncKeyState, GetFocus, IsWindowEnabled, SetFocus, VK_LBUTTON, VK_RBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    // Windows Terminal draws its grid below the tab row, inside its default padding.
    const TAB_ROW_DIP: f64 = 40.0;
    const PADDING_DIP: f64 = 8.0;
    const SCROLLBAR_DIP: f64 = 16.0;

    const SET_FULLSCREEN: u32 = WM_APP + 1;
    const CHILD_STYLE: u32 = WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN;
    static TERMINAL: AtomicIsize = AtomicIsize::new(0);
    static FULLSCREEN: AtomicBool = AtomicBool::new(false);

    /// Lifts the video out of the terminal to cover its monitor, or puts it back.
    /// It never takes activation, so the terminal keeps the keyboard either way.
    unsafe fn apply_fullscreen(hwnd: HWND, on: bool) {
        unsafe {
            let terminal = TERMINAL.load(Ordering::Relaxed) as HWND;
            if on {
                let mut info: MONITORINFO = std::mem::zeroed();
                info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                GetMonitorInfoW(MonitorFromWindow(terminal, MONITOR_DEFAULTTONEAREST), &mut info);
                let screen = info.rcMonitor;
                SetParent(hwnd, std::ptr::null_mut());
                SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_POPUP | WS_CLIPCHILDREN) as isize);
                SetWindowPos(hwnd, HWND_TOPMOST, screen.left, screen.top, screen.right - screen.left, screen.bottom - screen.top, SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED);
            } else {
                SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_HIDEWINDOW);
                SetWindowLongPtrW(hwnd, GWL_STYLE, CHILD_STYLE as isize);
                SetParent(hwnd, terminal);
                SetWindowPos(hwnd, HWND_TOP, 0, 0, 1, 1, SWP_NOACTIVATE | SWP_FRAMECHANGED);
            }
        }
    }

    unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe {
            if message == SET_FULLSCREEN {
                apply_fullscreen(hwnd, wparam != 0);
                return 0;
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The window the user sees: Windows Terminal owns the hidden helper console window.
    fn terminal_window() -> Option<(isize, bool)> {
        unsafe {
            let console = GetConsoleWindow();
            if console.is_null() {
                return None;
            }
            let owner = GetWindow(console, GW_OWNER);
            if !owner.is_null() {
                return Some((owner as isize, true));
            }
            (IsWindowVisible(console) != 0).then_some((console as isize, false))
        }
    }

    pub struct VideoHost {
        hwnd: isize,
        terminal: isize,
        tabbed: bool,
        placed: Mutex<Option<(i32, i32, i32, i32)>>,
        fullscreen: AtomicBool,
    }

    impl VideoHost {
        pub fn create() -> Option<Self> {
            let (terminal, tabbed) = terminal_window()?;
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
            TERMINAL.store(terminal, Ordering::Relaxed);
            let (tx, rx) = channel();
            // A child of another process's window needs its own thread pumping messages.
            std::thread::spawn(move || unsafe {
                let class = wide("ReelVideoHost");
                let instance = GetModuleHandleW(std::ptr::null());
                let definition = WNDCLASSW {
                    style: 0,
                    lpfnWndProc: Some(window_proc),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: instance,
                    hIcon: std::ptr::null_mut(),
                    hCursor: std::ptr::null_mut(),
                    hbrBackground: GetStockObject(BLACK_BRUSH) as HBRUSH,
                    lpszMenuName: std::ptr::null(),
                    lpszClassName: class.as_ptr(),
                };
                RegisterClassW(&definition);
                let hwnd = CreateWindowExW(
                    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                    class.as_ptr(),
                    std::ptr::null(),
                    CHILD_STYLE,
                    0,
                    0,
                    1,
                    1,
                    terminal as HWND,
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                let _ = tx.send(hwnd as isize);
                SetTimer(std::ptr::null_mut(), 0, 100, None);
                let mut typing_target = terminal as HWND;
                let mut on_top = true;
                let mut message: MSG = std::mem::zeroed();
                while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                    DispatchMessageW(&message);
                    // mpv disables its window under --wid, which would hide its on-screen
                    // controls from the mouse.
                    let video = GetWindow(hwnd, GW_CHILD);
                    if !video.is_null() && IsWindowEnabled(video) == 0 {
                        EnableWindow(video, 1);
                    }
                    // A fullscreen video stays on top only while reel is in use, so
                    // switching to another app works.
                    if FULLSCREEN.load(Ordering::Relaxed) {
                        let active = GetForegroundWindow();
                        let ours = active == terminal as HWND || active == hwnd || IsChild(hwnd, active) != 0;
                        if ours != on_top {
                            on_top = ours;
                            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
                            if ours {
                                SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, flags);
                            } else {
                                SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, flags);
                                SetWindowPos(hwnd, HWND_BOTTOM, 0, 0, 0, 0, flags);
                            }
                        }
                    } else {
                        on_top = true;
                    }
                    // A click on the video gives mpv the keyboard. Once the button is up
                    // (a seek drag needs it until then), hand it back so reel's keys work.
                    let focus = GetFocus();
                    if focus.is_null() {
                        continue;
                    }
                    if focus == hwnd || IsChild(hwnd, focus) != 0 {
                        if GetAsyncKeyState(VK_LBUTTON as i32) >= 0 && GetAsyncKeyState(VK_RBUTTON as i32) >= 0 {
                            SetFocus(typing_target);
                        }
                    } else {
                        typing_target = focus;
                    }
                }
            });
            let hwnd = rx.recv().ok().filter(|h| *h != 0)?;
            Some(Self { hwnd, terminal, tabbed, placed: Mutex::new(None), fullscreen: AtomicBool::new(false) })
        }

        pub fn wid(&self) -> isize {
            self.hwnd
        }

        fn pixels(&self, cells: Rect, cols: u16, rows: u16) -> (i32, i32, i32, i32) {
            let mut client = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            let dpi = unsafe {
                GetClientRect(self.terminal as HWND, &mut client);
                GetDpiForWindow(self.terminal as HWND)
            };
            let scale = dpi.max(96) as f64 / 96.0;
            let (top, pad, bar) = if self.tabbed { (TAB_ROW_DIP * scale, PADDING_DIP * scale, SCROLLBAR_DIP * scale) } else { (0.0, 0.0, 0.0) };
            let cell_w = (client.right as f64 - pad * 2.0 - bar) / cols.max(1) as f64;
            let cell_h = (client.bottom as f64 - top - pad * 2.0) / rows.max(1) as f64;
            let x = pad + cells.x as f64 * cell_w;
            let y = top + pad + cells.y as f64 * cell_h;
            (x.round() as i32, y.round() as i32, (cells.width as f64 * cell_w).round() as i32, (cells.height as f64 * cell_h).round() as i32)
        }

        pub fn set_fullscreen(&self, on: bool) {
            if self.fullscreen.swap(on, Ordering::Relaxed) != on {
                FULLSCREEN.store(on, Ordering::Relaxed);
                // Forget the old position so leaving fullscreen places it again.
                *self.placed.lock().unwrap() = None;
                unsafe { PostMessageW(self.hwnd as HWND, SET_FULLSCREEN, on as usize, 0) };
            }
        }

        /// Moves the video over these cells, or hides it with None.
        pub fn place(&self, cells: Option<Rect>, cols: u16, rows: u16) {
            if self.fullscreen.load(Ordering::Relaxed) {
                return;
            }
            let target = cells.map(|c| self.pixels(c, cols, rows));
            let mut placed = self.placed.lock().unwrap();
            if *placed == target {
                return;
            }
            *placed = target;
            let flags = SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
            unsafe {
                match target {
                    Some((x, y, w, h)) => SetWindowPos(self.hwnd as HWND, HWND_TOP, x, y, w, h, flags | SWP_SHOWWINDOW),
                    None => SetWindowPos(self.hwnd as HWND, HWND_TOP, 0, 0, 0, 0, flags | SWP_NOMOVE | SWP_NOSIZE | SWP_HIDEWINDOW),
                };
            }
        }
    }

    /// Windows Terminal gives each tab a hidden helper console window of 0x0. Minimizing
    /// the terminal grows it to a small box on the desktop, and restoring does not undo
    /// that. Keeps reel's own one at its normal size.
    pub fn hide_console_stub() {
        unsafe {
            let console = GetConsoleWindow();
            if console.is_null() || GetWindow(console, GW_OWNER).is_null() {
                return;
            }
            let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            if GetWindowRect(console, &mut rect) != 0 && rect.right > rect.left {
                SetWindowPos(console, std::ptr::null_mut(), 0, 0, 0, 0, SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);
            }
        }
    }
}

#[cfg(windows)]
pub use imp::{VideoHost, hide_console_stub};

#[cfg(not(windows))]
pub struct VideoHost;

#[cfg(not(windows))]
impl VideoHost {
    pub fn create() -> Option<Self> {
        None
    }

    pub fn wid(&self) -> isize {
        0
    }

    pub fn place(&self, _cells: Option<Rect>, _cols: u16, _rows: u16) {}

    pub fn set_fullscreen(&self, _on: bool) {}
}

#[cfg(not(windows))]
pub fn hide_console_stub() {}
