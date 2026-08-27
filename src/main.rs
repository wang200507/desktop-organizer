mod layout;
mod renderer;
mod scanner;
mod settings;

use renderer::Renderer;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT};
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_YESNO,
};
use windows::Win32::UI::Shell::{
    ShellExecuteW, SHGetDesktopFolder, IShellFolder, IShellView, IContextMenu,
    SVGIO_BACKGROUND, CMF_NORMAL, CMINVOKECOMMANDINFO,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use crate::layout::{CardStyle, ResizeKind};

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU32, Ordering};

/// 被隐藏的桌面 SysListView32 句柄，退出时恢复
static HIDDEN_DEFVIEW: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// 主窗口句柄（鼠标钩子回调通知用）
static MAIN_HWND: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
/// 本进程 PID
static OUR_PID: AtomicU32 = AtomicU32::new(0);
/// explorer（桌面层）PID
static EXPLORER_PID: AtomicU32 = AtomicU32::new(0);
/// 全局鼠标钩子句柄
static MOUSE_HOOK: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
/// 菜单打开标志：TrackPopupMenu 模态循环期间钩子透传，避免误判双击/误弹菜单
static MENU_OPEN: AtomicBool = AtomicBool::new(false);
/// 卸载中标志：WM_DESTROY 跳过布局/设置保存（数据目录已被清理）
static UNINSTALLING: AtomicBool = AtomicBool::new(false);

/// 判断窗口是否属于"桌面区域"（我们的窗口 或 explorer 桌面层，排除任务栏/开始按钮/弹出菜单）
fn is_desktop_area(hwnd_at: HWND) -> bool {
    if hwnd_at.is_invalid() {
        return false;
    }
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd_at, Some(&mut pid)); }
    if pid == OUR_PID.load(Ordering::SeqCst) {
        // 弹出菜单（类名 #32768）属于我们的进程，必须排除——否则钩子拦截菜单鼠标导致菜单无法选中/关闭
        let mut buf = [0u16; 64];
        let len = unsafe { GetClassNameW(hwnd_at, &mut buf) };
        if len > 0 {
            let class = String::from_utf16_lossy(&buf[..len as usize]);
            if class.starts_with('#') {
                return false; // 菜单/工具提示等 popup 窗口
            }
        }
        return true;
    }
    if pid == EXPLORER_PID.load(Ordering::SeqCst) {
        let mut buf = [0u16; 64];
        let len = unsafe { GetClassNameW(hwnd_at, &mut buf) };
        if len > 0 {
            let class = String::from_utf16_lossy(&buf[..len as usize]);
            // 排除任务栏/开始菜单等 explorer UI（它们的右键应保持自身菜单）
            if class.contains("TrayWnd") || class == "Start" {
                return false;
            }
        }
        return true;
    }
    false
}

/// 全局鼠标钩子：监听主屏桌面空白的右键，通知主窗口弹系统原生菜单
/// 卡片区域右键由主窗口弹自定义菜单；副屏右键由系统 SysListView32 原生处理
unsafe extern "system" fn ll_mouse_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if ncode == 0 {
        // 菜单打开期间（TrackPopupMenu 模态循环）：完全透传，避免误判双击/误弹菜单
        if MENU_OPEN.load(Ordering::SeqCst) {
            return CallNextHookEx(None, ncode, wparam, lparam);
        }
        let msll = lparam.0 as *const MSLLHOOKSTRUCT;
        if !msll.is_null() {
            let pt = (*msll).pt;
            let t = (*msll).time;
            let sw = GetSystemMetrics(SM_CXSCREEN);
            let sh = GetSystemMetrics(SM_CYSCREEN);
            if wparam.0 as u32 == WM_LBUTTONDOWN && pt.x >= 0 && pt.x < sw && pt.y >= 0 && pt.y < sh {
                // 双击检测：与上次 DOWN 时间差 < GetDoubleClickTime 且坐标接近
                let last = LAST_DOWN.load(Ordering::SeqCst);
                let dx = pt.x - LAST_DOWN_X.load(Ordering::SeqCst);
                let dy = pt.y - LAST_DOWN_Y.load(Ordering::SeqCst);
                LAST_DOWN.store(t, Ordering::SeqCst);
                LAST_DOWN_X.store(pt.x, Ordering::SeqCst);
                LAST_DOWN_Y.store(pt.y, Ordering::SeqCst);
                // 仅桌面区域（排除任务栏/开始按钮/应用窗口）才判定双击切换
                if is_desktop_area(WindowFromPoint(pt)) {
                    let dbl = GetDoubleClickTime() as u32;
                    if last != 0 && t.wrapping_sub(last) < dbl && dx.abs() <= 4 && dy.abs() <= 4 {
                        // 双击：不在卡片内 → 切换桌面可见性
                        let main = MAIN_HWND.load(Ordering::SeqCst);
                        if !main.is_null() {
                            let in_card = get_state(HWND(main)).map_or(false, |s| {
                                layout::hit_test(&s.cards, pt.x, pt.y).is_some()
                            });
                            if !in_card {
                                let _ = PostMessageW(Some(HWND(main)), MSG_TOGGLE, WPARAM(0), LPARAM(0));
                            }
                        }
                    }
                }
            }
            if wparam.0 as u32 == WM_RBUTTONUP && pt.x >= 0 && pt.x < sw && pt.y >= 0 && pt.y < sh {
                // 非桌面区域（正常应用窗口/任务栏）→ 不处理
                if is_desktop_area(WindowFromPoint(pt)) {
                    let main = MAIN_HWND.load(Ordering::SeqCst);
                    if !main.is_null() {
                        let in_card = get_state(HWND(main)).map_or(false, |s| {
                            layout::hit_test(&s.cards, pt.x, pt.y).is_some()
                        });
                        // 卡片区域右键由主窗口弹自定义菜单，这里不处理
                        if !in_card {
                            let _ = PostMessageW(
                                Some(HWND(main)),
                                MSG_SYSMENU,
                                WPARAM(pt.x as usize),
                                LPARAM(pt.y as isize),
                            );
                            // 阻止 explorer 自己的桌面右键菜单弹出（避免双菜单）
                            return LRESULT(1);
                        }
                    }
                }
            }
        }
    }
    CallNextHookEx(None, ncode, wparam, lparam)
}

/// 还原被隐藏的桌面图标列表 + 卸载鼠标钩子（WM_DESTROY 与 panic 兜底共用）
fn restore_desktop_state() {
    let dv = HIDDEN_DEFVIEW.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !dv.is_null() {
        let lv = HWND(dv);
        unsafe {
            ShowWindow(lv, SW_SHOW);
            InvalidateRect(Some(lv), None, true);
            UpdateWindow(lv);
        }
    }
    let hh = MOUSE_HOOK.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !hh.is_null() {
        unsafe { let _ = UnhookWindowsHookEx(HHOOK(hh)); }
    }
}

/// 查找桌面 SHELLDLL_DefView：Win10 直链 Progman，Win11 枚举 WorkerW 顶层窗口兜底
fn find_desktop_defview() -> Option<HWND> {
    unsafe {
        // Win10：Progman 直接子窗口
        if let Ok(progman) = FindWindowW(w!("Progman"), None) {
            if !progman.is_invalid() {
                if let Ok(dv) = FindWindowExW(Some(progman), None, w!("SHELLDLL_DefView"), None) {
                    if !dv.is_invalid() {
                        return Some(dv);
                    }
                }
            }
        }
        // Win11：SHELLDLL_DefView 藏在 WorkerW 顶层窗口下，枚举所有顶层窗口找
        let mut found: Option<HWND> = None;
        unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> windows::Win32::Foundation::BOOL {
            let slot = lparam.0 as *mut Option<HWND>;
            if let Ok(dv) = FindWindowExW(Some(hwnd), None, w!("SHELLDLL_DefView"), None) {
                if !dv.is_invalid() {
                    unsafe { *slot = Some(dv) };
                    return windows::Win32::Foundation::BOOL(0);
                }
            }
            windows::Win32::Foundation::BOOL(1)
        }
        let slot: *mut Option<HWND> = &mut found;
        let _ = EnumWindows(Some(enum_cb), LPARAM(slot as isize));
        found
    }
}

#[link(name = "user32")]
extern "system" {
    fn SetCapture(hwnd: HWND) -> HWND;
    fn ReleaseCapture() -> i32;
}

/// 用 Shell COM 接口弹出系统原生桌面右键菜单（查看/排序/刷新/新建/显示设置/个性化等）
/// 不依赖被隐藏的 SysListView32 窗口，通过桌面 IShellFolder → IShellView → 背景 IContextMenu 获取
fn show_system_desktop_menu(hwnd: HWND, x: i32, y: i32) {
    unsafe {
        let desktop: IShellFolder = match SHGetDesktopFolder() {
            Ok(d) => d,
            Err(e) => { error_log(&format!("SHGetDesktopFolder err={}", e)); return; }
        };
        let view: IShellView = match desktop.CreateViewObject(hwnd) {
            Ok(v) => v,
            Err(e) => { error_log(&format!("CreateViewObject err={}", e)); return; }
        };
        let ctx: IContextMenu = match view.GetItemObject(SVGIO_BACKGROUND) {
            Ok(c) => c,
            Err(e) => { error_log(&format!("GetItemObject err={}", e)); return; }
        };
        let menu = match CreatePopupMenu() {
            Ok(m) => m,
            Err(_) => return,
        };
        const CMD_FIRST: u32 = 1;
        const CMD_LAST: u32 = 0x7fff;
        let hr = ctx.QueryContextMenu(menu, 0, CMD_FIRST, CMD_LAST, CMF_NORMAL);
        if hr.is_ok() {
            // TPM_RETURNCMD：菜单选中的命令 ID 作为返回值；TPM_NONOTIFY：不发送 WM_COMMAND，自己 InvokeCommand
            let cmd = TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                x,
                y,
                None,
                hwnd,
                None,
            );
            let cmd_id = cmd.0 as u32;
            if cmd_id >= CMD_FIRST && cmd_id <= CMD_LAST {
                let mut ici = CMINVOKECOMMANDINFO::default();
                ici.cbSize = std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32;
                ici.hwnd = hwnd;
                // MAKEINTRESOURCEA(idCmdOffset)：把命令偏移量当作 verb 指针
                ici.lpVerb = PCSTR((cmd_id - CMD_FIRST) as isize as *const u8);
                ici.nShow = SW_SHOWNORMAL.0;
                let _ = ctx.InvokeCommand(&ici);
            }
        }
        let _ = DestroyMenu(menu);
    }
}

/// 数据目录：%LOCALAPPDATA%\DesktopOrganizer（layout/settings/error.log 统一存放，卸载时整体删除）
fn data_dir() -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".to_string());
    let dir = std::path::PathBuf::from(base).join("DesktopOrganizer");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 错误日志：只记录错误（panic / Shell COM 失败等），写入数据目录 error.log
fn error_log(msg: &str) {
    use std::io::Write;
    let path = data_dir().join("error.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", msg);
    }
}

/// 开机自启状态（注册表 HKCU\Software\Microsoft\Windows\CurrentVersion\Run\DesktopOrganizer）
fn autostart_enabled() -> bool {
    unsafe {
        let key_path = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, key_path, None, KEY_QUERY_VALUE, &mut hkey).is_ok() {
            let name = w!("DesktopOrganizer");
            let mut cb: u32 = 0;
            let r = RegQueryValueExW(hkey, name, None, None, None, Some(&mut cb));
            RegCloseKey(hkey);
            r.is_ok()
        } else {
            false
        }
    }
}

/// 设置开机自启：enabled=true 写 Run 键（exe 路径），false 删除
fn set_autostart(enabled: bool) {
    unsafe {
        let key_path = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, key_path, None, KEY_SET_VALUE, &mut hkey).is_err() {
            error_log("set_autostart: RegOpenKeyExW 失败");
            return;
        }
        let name = w!("DesktopOrganizer");
        if enabled {
            if let Ok(exe) = std::env::current_exe() {
                let s = format!("\"{}\"", exe.to_string_lossy());
                let mut w: Vec<u16> = s.encode_utf16().collect();
                w.push(0); // REG_SZ 需以 NUL 结尾
                let bytes: &[u8] =
                    std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 2);
                let _ = RegSetValueExW(hkey, name, None, REG_SZ, Some(bytes));
            }
        } else {
            let _ = RegDeleteValueW(hkey, name);
        }
        RegCloseKey(hkey);
    }
}

/// 卸载：删自启键 + 还原桌面图标 + 清理数据目录 + 标记 exe 重启后自动删除
fn uninstall() {
    UNINSTALLING.store(true, Ordering::SeqCst);
    set_autostart(false);
    // 还原桌面图标（若程序正在接管中）
    let dv = HIDDEN_DEFVIEW.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !dv.is_null() {
        unsafe {
            let lv = HWND(dv);
            ShowWindow(lv, SW_SHOW);
            InvalidateRect(Some(lv), None, true);
            UpdateWindow(lv);
        }
    }
    // 清理数据目录（layout.json / settings.json / error.log）
    let _ = std::fs::remove_dir_all(data_dir());
    // 运行中的 exe 无法直接删除：标记重启后自动删除
    if let Ok(exe) = std::env::current_exe() {
        let s: Vec<u16> = exe.to_string_lossy().encode_utf16().collect();
        unsafe {
            let _ = MoveFileExW(
                PCWSTR(s.as_ptr()),
                PCWSTR(std::ptr::null()),
                MOVEFILE_DELAY_UNTIL_REBOOT,
            );
        }
    }
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

struct State {
    renderer: Renderer,
    items: Vec<scanner::DesktopItem>,
    cards: Vec<layout::Card>,
    drag: Option<DragState>,
    visible: bool,
    menu_card: Option<usize>,
    settings: settings::Settings,
    settings_hwnd: Option<HWND>,
    last_render: std::time::Instant,
    selected: Option<(usize, usize)>,
}

#[derive(Clone, Copy, PartialEq)]
enum DragMode {
    Move,
    ResizeRight,
    ResizeBottom,
    ResizeCorner,
}

struct DragState {
    card_index: usize,
    mode: DragMode,
    offset_x: i32,  // Move 用：鼠标相对卡片左上角偏移
    offset_y: i32,
    start_x: i32,   // Resize 用：按下时鼠标位置
    start_y: i32,
    start_w: i32,   // Resize 用：按下时卡片尺寸
    start_h: i32,
}

struct SettingsState {
    main_hwnd: HWND,
    settings: settings::Settings,
}

const CMD_DELETE: usize = 1;
const CMD_NEW: usize = 2;
const CMD_EXIT: usize = 3;
const CMD_SETTINGS: usize = 4;
const CMD_DISPLAY: usize = 5;
/// 全局鼠标钩子转发桌面空白右键的自定义消息
const MSG_SYSMENU: u32 = WM_APP + 1;
/// 全局鼠标钩子检测双击空白的自定义消息（切换桌面可见性）
const MSG_TOGGLE: u32 = WM_APP + 2;
/// Win+D 兜底定时器 ID（每秒检查窗口可见性并恢复）
const TIMER_ID: usize = 1;
/// 菜单关闭后补渲染（菜单打开期间跳过 render，避免模态中移动窗口干扰菜单）
const MSG_RENDER: u32 = WM_APP + 3;

/// 双击检测：上次左键按下的时间/坐标（LL 钩子不报 DBLCLK，需自己判定）
static LAST_DOWN: AtomicU32 = AtomicU32::new(0);
static LAST_DOWN_X: AtomicI32 = AtomicI32::new(i32::MIN);
static LAST_DOWN_Y: AtomicI32 = AtomicI32::new(i32::MIN);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // 卸载入口：清理自启/数据目录/桌面接管状态，exe 标记重启后删除
    if args.len() >= 2 && args[1] == "--uninstall" {
        uninstall();
        return;
    }
    if args.len() >= 3 && args[1] == "--watchdog" {
        // Watchdog 模式：等待主进程退出（含任务管理器强杀），然后还原桌面图标
        let pid: u32 = args[2].parse().unwrap_or(0);
        unsafe {
            if let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                if !handle.is_invalid() {
                    let _ = WaitForSingleObject(handle, u32::MAX);
                    CloseHandle(handle);
                }
            }
            // 还原被隐藏的 SysListView32
            if let Some(defview) = find_desktop_defview() {
                if let Ok(listview) = FindWindowExW(Some(defview), None, w!("SysListView32"), None) {
                    if !listview.is_invalid() {
                        ShowWindow(listview, SW_SHOW);
                        InvalidateRect(Some(listview), None, true);
                        UpdateWindow(listview);
                    }
                }
            }
        }
        return;
    }
    // 异常退出（panic）时兜底还原桌面图标 + 记录错误日志
    std::panic::set_hook(Box::new(|info| {
        restore_desktop_state();
        error_log(&format!("panic: {}", info));
    }));
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        // 隐藏 console 窗口，让 GUI 分层窗口可见
        let console = windows::Win32::System::Console::GetConsoleWindow();
        if !console.is_invalid() {
            ShowWindow(console, SW_HIDE);
        }
    }

    let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };

    let items = scanner::scan_desktop();

    println!("\n=== 桌面图标扫描结果 ===");
    println!("共 {} 个项", items.len());
    print!("分类: ");
    for (k, c) in scanner::count_by_kind(&items) {
        print!("{}{} ", k.label(), c);
    }
    println!();
    for (i, card) in layout::classify(&items).iter().enumerate() {
        println!("  分区{}: {} {} 个", i + 1, card.title, card.item_indices.len());
    }
    println!("=========================\n");

    let hinst = unsafe { GetModuleHandleW(None).expect("GetModuleHandleW") };
    let class_name = w!("DeskOrgWnd");

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinst.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).expect("LoadCursorW") },
        lpszClassName: class_name,
        ..Default::default()
    };
    let atom = unsafe { RegisterClassExW(&wc) };
    assert!(atom != 0, "RegisterClassExW 失败");

    // 设置窗口类
    let settings_class = w!("DeskOrgSettings");
    let swc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(settings_proc),
        hInstance: hinst.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).expect("LoadCursorW") },
        lpszClassName: settings_class,
        ..Default::default()
    };
    let satom = unsafe { RegisterClassExW(&swc) };
    assert!(satom != 0, "RegisterClassExW 设置窗口失败");

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED,
            class_name,
            w!("桌面图标整理"),
            WS_POPUP,
            0,
            0,
            sw,
            sh,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .expect("CreateWindowExW 失败")
    };

    unsafe {
        // 先还原上次可能遗留的隐藏状态（如上次被任务管理器强杀），再重新接管
        restore_desktop_state();
        // 初始化 PID 与全局鼠标钩子（桌面空白右键监听）
        OUR_PID.store(GetCurrentProcessId(), Ordering::SeqCst);
        MAIN_HWND.store(hwnd.0, Ordering::SeqCst);
        if let Ok(progman) = FindWindowW(w!("Progman"), None) {
            if !progman.is_invalid() {
                let mut epid: u32 = 0;
                GetWindowThreadProcessId(progman, Some(&mut epid));
                EXPLORER_PID.store(epid, Ordering::SeqCst);
            }
        }
        if let Ok(h) = SetWindowsHookExW(WH_MOUSE_LL, Some(ll_mouse_proc), Some(hinst.into()), 0) {
            MOUSE_HOOK.store(h.0, Ordering::SeqCst);
        }
        // 隐藏桌面图标列表（SysListView32 窗口本身）；桌面右键由全局钩子 + Shell COM 接管
        if let Some(defview) = find_desktop_defview() {
            match FindWindowExW(Some(defview), None, w!("SysListView32"), None) {
                Ok(listview) => {
                    if !listview.is_invalid() {
                        ShowWindow(listview, SW_HIDE);
                        HIDDEN_DEFVIEW.store(listview.0, Ordering::SeqCst);
                    }
                }
                Err(_) => {}
            }
            // 放到 WorkerW 之后（壁纸之上、正常窗口之下），不改变父子关系
            match GetParent(defview) {
                Ok(workerw) => {
                    if !workerw.is_invalid() {
                        SetWindowPos(
                            hwnd,
                            Some(workerw),
                            0,
                            0,
                            0,
                            0,
                            SET_WINDOW_POS_FLAGS(0x0001 | 0x0002 | 0x0010),
                        );
                    }
                }
                Err(e) => error_log(&format!("GetParent err={}", e)),
            }
        }
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        // 显式首次渲染（分层窗口不走 WM_PAINT，避免重启后空白需点击才出现）
        if let Some(s) = get_state(hwnd) {
            s.renderer.render(hwnd, &s.cards, &s.items, s.visible, s.settings.alpha, s.settings.show_icons, s.selected);
        }
    }

    // 启动 watchdog 子进程：主进程异常退出（任务管理器强杀）时还原桌面图标
    {
        use std::os::windows::process::CommandExt;
        if let Ok(exe) = std::env::current_exe() {
            let pid = std::process::id();
            let _ = std::process::Command::new(exe)
                .arg("--watchdog")
                .arg(pid.to_string())
                .creation_flags(0x08000000) // CREATE_NO_WINDOW：不弹 console
                .spawn();
        }
    }

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0).as_bool() } {
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unsafe {
        // 卸载全局鼠标钩子
        let hh = MOUSE_HOOK.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !hh.is_null() {
            let _ = UnhookWindowsHookEx(HHOOK(hh));
        }
        CoUninitialize();
    }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let items = scanner::scan_desktop();
            let mut cards = layout::classify(&items);
            let cw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
            let ch = unsafe { GetSystemMetrics(SM_CYSCREEN) };
            let mut settings = settings::load_settings(
                data_dir().join("settings.json").to_str().unwrap_or("settings.json"),
            );
            // 自启状态以注册表实际为准（避免设置文件与注册表不同步）
            settings.autostart = autostart_enabled();
            layout::layout_cards(&mut cards, cw, ch, settings.card_cols as usize);
            {
                let lp = data_dir().join("layout.json");
                layout::load_layout(&mut cards, lp.to_str().unwrap_or("layout.json"));
            }
            let mut renderer = Renderer::new();
            renderer.set_bg_color(settings.bg_color);
            let state = Box::new(State {
                renderer,
                items,
                cards,
                drag: None,
                visible: true,
                menu_card: None,
                settings,
                settings_hwnd: None,
                last_render: std::time::Instant::now(),
                selected: None,
            });
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
                InvalidateRect(Some(hwnd), None, true);
                // Win+D 兜底定时器：每秒检查窗口可见性
                SetTimer(Some(hwnd), TIMER_ID, 1000, None);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_NCHITTEST => {
            // 全部返回 HTCLIENT：卡片区可拖拽交互，透明区左键不处理（等效穿透）、右键弹菜单
            LRESULT(HTCLIENT as isize)
        }
        WM_SETCURSOR => {
            // 边缘显示 resize 光标
            let mut pt = POINT::default();
            unsafe { GetCursorPos(&mut pt); }
            if let Some(s) = get_state(hwnd) {
                if let Some((_, kind)) = layout::hit_test_resize(&s.cards, pt.x, pt.y) {
                    let cursor_id = match kind {
                        layout::ResizeKind::Right => IDC_SIZEWE,
                        layout::ResizeKind::Bottom => IDC_SIZENS,
                        layout::ResizeKind::Corner => IDC_SIZENWSE,
                    };
                    if let Ok(cursor) = unsafe { LoadCursorW(None, cursor_id) } {
                        unsafe { SetCursor(Some(cursor)); }
                    }
                    return LRESULT(1);
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = lparam_to_screen(hwnd, lp);
            if let Some(s) = get_state(hwnd) {
                // 双击图标项 → 打开文件/快捷方式
                if let Some((ci, ii)) = layout::hit_test_item(&s.cards, x, y, s.settings.show_icons) {
                    if let Some(card) = s.cards.get(ci) {
                        if let Some(&item_idx) = card.item_indices.get(ii) {
                            if let Some(item) = s.items.get(item_idx) {
                                let path_str = item.path.to_string_lossy();
                                let path_w: Vec<u16> = path_str.encode_utf16().collect();
                                unsafe {
                                    ShellExecuteW(
                                        None,
                                        w!("open"),
                                        PCWSTR(path_w.as_ptr()),
                                        None,
                                        None,
                                        SW_SHOW,
                                    );
                                }
                            }
                        }
                    }
                } else if s.settings.double_click_hide && layout::hit_test(&s.cards, x, y).is_none() {
                    // 双击卡片外空白 → 隐藏/显示
                    s.visible = !s.visible;
                    s.renderer.render(hwnd, &s.cards, &s.items, s.visible, s.settings.alpha, s.settings.show_icons, s.selected);
                }
            }
            LRESULT(0)
        }
        MSG_SYSMENU => {
            // 全局鼠标钩子转发的桌面空白右键 → 弹系统原生桌面菜单
            let x = wp.0 as i32;
            let y = lp.0 as i32;
            show_system_desktop_menu(hwnd, x, y);
            LRESULT(0)
        }
        MSG_TOGGLE => {
            // 钩子检测到双击桌面空白 → 切换可见性（隐藏/显示卡片）
            if let Some(s) = get_state(hwnd) {
                s.visible = !s.visible;
                s.renderer.render(hwnd, &s.cards, &s.items, s.visible, s.settings.alpha, s.settings.show_icons, s.selected);
            }
            LRESULT(0)
        }
        MSG_RENDER => {
            // 菜单关闭后补渲染（菜单打开期间跳过 render，避免模态中移动窗口干扰菜单）
            if let Some(s) = get_state(hwnd) {
                s.renderer.render(hwnd, &s.cards, &s.items, s.visible, s.settings.alpha, s.settings.show_icons, s.selected);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // 菜单打开期间不处理滚轮（避免模态循环中渲染干扰菜单）
            if MENU_OPEN.load(Ordering::SeqCst) {
                return LRESULT(0);
            }
            // 滚轮滚动列表（lParam 是屏幕坐标）
            let x = lparam_x(lp);
            let y = lparam_y(lp);
            let delta = ((wp.0 as u32 >> 16) & 0xffff) as u16 as i16;
            if let Some(s) = get_state(hwnd) {
                if let Some(idx) = layout::hit_test(&s.cards, x, y) {
                    if let Some(card) = s.cards.get_mut(idx) {
                        // 网格/列表都支持滚轮滚动
                        card.scroll -= (delta / 120) as i32; // 上滚负数方向调整
                        s.renderer.render(hwnd, &s.cards, &s.items, s.visible, s.settings.alpha, s.settings.show_icons, s.selected);
                    }
                }
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            match wp.0 {
                0x70 => {
                    // F1 打开设置窗口
                    if let Some(s) = get_state(hwnd) {
                        if s.settings_hwnd.is_none() {
                            let hinst = unsafe { GetModuleHandleW(None).expect("GetModuleHandleW") };
                            let shwnd = unsafe {
                                CreateWindowExW(
                                    WINDOW_EX_STYLE::default(),
                                    w!("DeskOrgSettings"),
                                    w!("设置"),
                                    WS_OVERLAPPEDWINDOW,
                                    CW_USEDEFAULT,
                                    CW_USEDEFAULT,
                                    340,
                                    440,
                                    None,
                                    None,
                                    Some(hinst.into()),
                                    None,
                                )
                                .expect("设置窗口失败")
                            };
                            let ss = Box::new(SettingsState {
                                main_hwnd: hwnd,
                                settings: s.settings.clone(),
                            });
                            unsafe {
                                SetWindowLongPtrW(shwnd, GWLP_USERDATA, Box::into_raw(ss) as isize);
                                ShowWindow(shwnd, SW_SHOW);
                                UpdateWindow(shwnd);
                            }
                            s.settings_hwnd = Some(shwnd);
                        }
                    }
                }
                0x74 => {
                    // F5 一键自动归位
                    if let Some(s) = get_state(hwnd) {
                        let cw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
                        let ch = unsafe { GetSystemMetrics(SM_CYSCREEN) };
                        layout::layout_cards(&mut s.cards, cw, ch, s.settings.card_cols as usize);
                        s.renderer.render(hwnd, &s.cards, &s.items, s.visible, s.settings.alpha, s.settings.show_icons, s.selected);
                    }
                }
                0x1B => {
                    // Esc 退出
                    unsafe { DestroyWindow(hwnd) };
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = lparam_to_screen(hwnd, lp);
            if let Some(s) = get_state(hwnd) {
                let idx = layout::hit_test(&s.cards, x, y);
                s.menu_card = idx;
                match idx {
                    Some(_) => {
                        // 卡片内：自定义菜单（删除/新建/设置/退出）
                        // 弹菜单期间置 MENU_OPEN，钩子透传，避免菜单点击被误判双击/拦截
                        MENU_OPEN.store(true, Ordering::SeqCst);
                        let menu = unsafe { CreatePopupMenu().expect("CreatePopupMenu") };
                        unsafe {
                            let _ = AppendMenuW(menu, MF_STRING, CMD_DELETE, w!("删除此分区"));
                            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                            let _ = AppendMenuW(menu, MF_STRING, CMD_NEW, w!("新建分区"));
                            let _ = AppendMenuW(menu, MF_STRING, CMD_SETTINGS, w!("设置"));
                            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                            let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("退出程序"));
                            TrackPopupMenu(menu, TPM_LEFTALIGN | TPM_RIGHTBUTTON, x, y, None, hwnd, None);
                            let _ = DestroyMenu(menu);
                        }
                        MENU_OPEN.store(false, Ordering::SeqCst);
                        // 菜单期间可能有数据变更（删除/新建分区），补一帧渲染
                        unsafe {
                            let _ = PostMessageW(Some(hwnd), MSG_RENDER, WPARAM(0), LPARAM(0));
                        }
                    }
                    None => {
                        // 桌面空白右键由全局鼠标钩子统一处理（弹系统原生菜单），这里不处理
                    }
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let cmd = (wp.0 as u32 & 0xffff) as usize;
            if let Some(s) = get_state(hwnd) {
                let mut changed = false;
                match cmd {
                    CMD_DELETE => {
                        if let Some(idx) = s.menu_card {
                            layout::remove_card(&mut s.cards, idx);
                            changed = true;
                        }
                    }
                    CMD_NEW => {
                        layout::add_card(&mut s.cards);
                        changed = true;
                    }
                    CMD_EXIT => {
                        unsafe { DestroyWindow(hwnd) };
                    }
                    CMD_SETTINGS => {
                        // 打开设置窗口（模拟 F1）
                        unsafe { let _ = PostMessageW(Some(hwnd), WM_KEYDOWN, WPARAM(0x70), LPARAM(0)); }
                    }
                    CMD_DISPLAY => {
                        // 打开系统显示设置
                        unsafe { ShellExecuteW(None, w!("open"), w!("ms-settings:display"), None, None, SW_SHOW); }
                    }
                    _ => {}
                }
                s.menu_card = None;
                if changed {
                    let cw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
                    let ch = unsafe { GetSystemMetrics(SM_CYSCREEN) };
                    layout::layout_cards(&mut s.cards, cw, ch, s.settings.card_cols as usize);
                    // 菜单可能还在模态循环中（WM_COMMAND 重入）：跳过 render，由 MSG_RENDER 在菜单关闭后补
                    if !MENU_OPEN.load(Ordering::SeqCst) {
                        s.renderer.render(hwnd, &s.cards, &s.items, s.visible, s.settings.alpha, s.settings.show_icons, s.selected);
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = lparam_to_screen(hwnd, lp);
            if let Some(s) = get_state(hwnd) {
                if let Some(idx) = layout::hit_test_close(&s.cards, x, y) {
                    // 点击 X 删除分区
                    layout::remove_card(&mut s.cards, idx);
                    if s.cards.is_empty() {
                        // 无卡片：还原原生桌面并退出
                        unsafe { DestroyWindow(hwnd) };
                        return LRESULT(0);
                    }
                    let cw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
                    let ch = unsafe { GetSystemMetrics(SM_CYSCREEN) };
                    layout::layout_cards(&mut s.cards, cw, ch, s.settings.card_cols as usize);
                    s.renderer.render(hwnd, &s.cards, &s.items, s.visible, s.settings.alpha, s.settings.show_icons, s.selected);
                } else if let Some(idx) = layout::hit_test_style(&s.cards, x, y) {
                    // 点击样式切换按钮：网格/列表互切
                    s.cards[idx].style = match s.cards[idx].style {
                        CardStyle::Grid => CardStyle::List,
                        CardStyle::List => CardStyle::Grid,
                    };
                    s.cards[idx].scroll = 0;
                    s.renderer.render(hwnd, &s.cards, &s.items, s.visible, s.settings.alpha, s.settings.show_icons, s.selected);
                } else if let Some((idx, kind)) = layout::hit_test_resize(&s.cards, x, y) {
                    // 置顶：把该卡片移到数组末尾（z-order 上层），避免被其他卡片盖住
                    let card = s.cards.remove(idx);
                    s.cards.push(card);
                    let idx = s.cards.len() - 1;
                    // 边缘拖拽调整大小
                    let card = &s.cards[idx];
                    let mode = match kind {
                        layout::ResizeKind::Right => DragMode::ResizeRight,
                        layout::ResizeKind::Bottom => DragMode::ResizeBottom,
                        layout::ResizeKind::Corner => DragMode::ResizeCorner,
                    };
                    s.drag = Some(DragState {
                        card_index: idx,
                        mode,
                        offset_x: 0,
                        offset_y: 0,
                        start_x: x,
                        start_y: y,
                        start_w: card.width,
                        start_h: card.height,
                    });
                    unsafe { SetCapture(hwnd); }
                } else if let Some((ci, ii)) = layout::hit_test_item(&s.cards, x, y, s.settings.show_icons) {
                    // 单击选中图标项（渲染延迟到 UP，避免双击时重复渲染拖慢打开）
                    s.selected = Some((ci, ii));
                } else if let Some(idx) = layout::hit_test_title(&s.cards, x, y) {
                    // 置顶：把该卡片移到数组末尾（z-order 上层），避免被其他卡片盖住
                    let card = s.cards.remove(idx);
                    s.cards.push(card);
                    let idx = s.cards.len() - 1;
                    // 标题栏拖拽移动
                    let card = &s.cards[idx];
                    s.drag = Some(DragState {
                        card_index: idx,
                        mode: DragMode::Move,
                        offset_x: x - card.x,
                        offset_y: y - card.y,
                        start_x: 0,
                        start_y: 0,
                        start_w: 0,
                        start_h: 0,
                    });
                    unsafe { SetCapture(hwnd); }
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = lparam_to_screen(hwnd, lp);
            if let Some(s) = get_state(hwnd) {
                if let Some(drag) = &s.drag {
                    let idx = drag.card_index;
                    const MIN_W: i32 = 120;
                    const MIN_H: i32 = 80;
                    const MAX_W: i32 = 640;
                    const MAX_H: i32 = 420;
                    match drag.mode {
                        DragMode::Move => {
                            let cw = s.cards[idx].width;
                            let ch = s.cards[idx].height;
                            let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
                            let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
                            // 允许卡片重叠（拖动时置顶保证可选中），仅限制屏幕边界
                            s.cards[idx].x = (x - drag.offset_x).clamp(0, (sw - cw).max(0));
                            s.cards[idx].y = (y - drag.offset_y).clamp(0, (sh - ch).max(0));
                        }
                        DragMode::ResizeRight => {
                            let nw = (drag.start_w + x - drag.start_x).clamp(MIN_W, MAX_W);
                            s.cards[idx].width = nw;
                        }
                        DragMode::ResizeBottom => {
                            let nh = (drag.start_h + y - drag.start_y).clamp(MIN_H, MAX_H);
                            s.cards[idx].height = nh;
                        }
                        DragMode::ResizeCorner => {
                            let nw = (drag.start_w + x - drag.start_x).clamp(MIN_W, MAX_W);
                            let nh = (drag.start_h + y - drag.start_y).clamp(MIN_H, MAX_H);
                            s.cards[idx].width = nw;
                            s.cards[idx].height = nh;
                        }
                    }
                    if s.last_render.elapsed().as_millis() >= 16 {
                        s.last_render = std::time::Instant::now();
                        s.renderer.render(hwnd, &s.cards, &s.items, s.visible, s.settings.alpha, s.settings.show_icons, s.selected);
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            unsafe { ReleaseCapture(); }
            if let Some(s) = get_state(hwnd) {
                s.drag = None;
                s.renderer.render(hwnd, &s.cards, &s.items, s.visible, s.settings.alpha, s.settings.show_icons, s.selected);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            // 分层窗口不走 GDI 绘制：所有渲染由显式 render() 通过 UpdateLayeredWindow 提交。
            // 这里只验证区域，避免 WM_PAINT → render → SetWindowPos → WM_SIZE → InvalidateRect
            // → WM_PAINT 重绘风暴（白屏闪烁 + 消息队列积压卡死 + CPU 高）。
            unsafe { ValidateRect(Some(hwnd), None) };
            LRESULT(0)
        }
        WM_WINDOWPOSCHANGING => {
            // 拦截系统隐藏（Win+D 显示桌面会向顶层窗口发 SWP_HIDEWINDOW）：卡片保持可见
            // visible=false 时窗口也要显示"恢复提示"，故无条件拦截
            let wpos = lp.0 as *mut WINDOWPOS;
            if !wpos.is_null() {
                unsafe {
                    let f = (*wpos).flags;
                    if f.0 & SWP_HIDEWINDOW.0 != 0 {
                        (*wpos).flags = SET_WINDOW_POS_FLAGS(f.0 & !SWP_HIDEWINDOW.0);
                    }
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            // 兜底：Win+D 等系统隐藏后自动恢复可见（每秒检查一次，开销极小）
            if wp.0 == TIMER_ID {
                if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
                    unsafe { ShowWindow(hwnd, SW_SHOW) };
                }
            }
            LRESULT(0)
        }
        WM_SHOWWINDOW => {
            // 拦截被隐藏（如 Win+D 显示桌面会把顶层窗口最小化）：卡片保持可见
            if wp.0 == 0 {
                if let Some(s) = get_state(hwnd) {
                    if s.visible {
                        unsafe { ShowWindow(hwnd, SW_SHOW) };
                    }
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { let _ = KillTimer(Some(hwnd), TIMER_ID); }
            if !UNINSTALLING.load(Ordering::SeqCst) {
                if let Some(s) = get_state(hwnd) {
                    let lp = data_dir().join("layout.json");
                    layout::save_layout(&s.cards, lp.to_str().unwrap_or("layout.json"));
                    let sp = data_dir().join("settings.json");
                    settings::save_settings(&s.settings, sp.to_str().unwrap_or("settings.json"));
                }
            }
            // 还原桌面图标列表 + 卸载钩子
            restore_desktop_state();
            let p = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            if p != 0 {
                let state = unsafe { Box::from_raw(p as *mut State) };
                // 释放图标句柄
                for it in &state.items {
                    if let Some(icon) = it.icon {
                        unsafe { let _ = DestroyIcon(icon); }
                    }
                }
                drop(state);
            }
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

extern "system" fn settings_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
            if p != 0 {
                let state = unsafe { &mut *(p as *mut SettingsState) };
                draw_settings(hwnd, &state.settings);
            } else {
                unsafe { ValidateRect(Some(hwnd), None) };
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let y = lparam_y(lp);
            let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
            if p != 0 {
                let state = unsafe { &mut *(p as *mut SettingsState) };
                let mut changed = false;
                if y >= 10 && y < 56 {
                    state.settings.alpha = match state.settings.alpha {
                        200 => 180,
                        180 => 160,
                        160 => 140,
                        140 => 120,
                        _ => 200,
                    };
                    changed = true;
                } else if y >= 56 && y < 102 {
                    state.settings.double_click_hide = !state.settings.double_click_hide;
                    changed = true;
                } else if y >= 102 && y < 148 {
                    state.settings.auto_classify = !state.settings.auto_classify;
                    changed = true;
                } else if y >= 148 && y < 194 {
                    state.settings.show_icons = !state.settings.show_icons;
                    changed = true;
                } else if y >= 194 && y < 240 {
                    state.settings.card_cols = match state.settings.card_cols {
                        3 => 2,
                        2 => 4,
                        _ => 3,
                    };
                    changed = true;
                } else if y >= 240 && y < 286 {
                    // 背景色循环切换预设色
                    state.settings.bg_color = match state.settings.bg_color {
                        0x262A36 => 0x3A3A4A,
                        0x3A3A4A => 0x4A3A3A,
                        0x4A3A3A => 0x3A4A3A,
                        _ => 0x262A36,
                    };
                    changed = true;
                } else if y >= 286 && y < 332 {
                    // 开机自启开关（写/删注册表 Run 键）
                    state.settings.autostart = !state.settings.autostart;
                    set_autostart(state.settings.autostart);
                    changed = true;
                } else if y >= 332 && y < 378 {
                    // 卸载程序：确认后清理并退出
                    let msg_w: Vec<u16> = "确定要卸载吗？\n将删除：开机自启、数据目录（布局/设置/日志）。\n程序 exe 将在重启系统后自动删除。".encode_utf16().collect();
                    let cap_w: Vec<u16> = "卸载确认".encode_utf16().collect();
                    let ret = unsafe {
                        MessageBoxW(
                            Some(hwnd),
                            PCWSTR(msg_w.as_ptr()),
                            PCWSTR(cap_w.as_ptr()),
                            MB_YESNO,
                        )
                    };
                    if ret == IDYES {
                        uninstall();
                        let main = state.main_hwnd;
                        unsafe { DestroyWindow(hwnd) };
                        unsafe { DestroyWindow(main) };
                        return LRESULT(0);
                    }
                }
                if changed {
                    let sp = data_dir().join("settings.json");
                    settings::save_settings(&state.settings, sp.to_str().unwrap_or("settings.json"));
                    if let Some(s) = get_state(state.main_hwnd) {
                        let cols_changed = s.settings.card_cols != state.settings.card_cols;
                        let bg_changed = s.settings.bg_color != state.settings.bg_color;
                        s.settings = state.settings.clone();
                        if cols_changed {
                            let cw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
                            let ch = unsafe { GetSystemMetrics(SM_CYSCREEN) };
                            layout::layout_cards(&mut s.cards, cw, ch, s.settings.card_cols as usize);
                        }
                        if bg_changed {
                            s.renderer.set_bg_color(s.settings.bg_color);
                        }
                        s.renderer.render(state.main_hwnd, &s.cards, &s.items, s.visible, s.settings.alpha, s.settings.show_icons, s.selected);
                    }
                    unsafe { InvalidateRect(Some(hwnd), None, true) };
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let p = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            if p != 0 {
                let state = unsafe { Box::from_raw(p as *mut SettingsState) };
                if let Some(s) = get_state(state.main_hwnd) {
                    s.settings_hwnd = None;
                }
                drop(state);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn draw_settings(hwnd: HWND, settings: &settings::Settings) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if hdc.is_invalid() {
            return;
        }
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let cw = rc.right - rc.left;
        let ch = rc.bottom - rc.top;
        // 背景
        let bg = CreateSolidBrush(rgb(245, 246, 250));
        let full = RECT { left: 0, top: 0, right: cw, bottom: ch };
        FillRect(hdc, &full, bg);
        DeleteObject(bg.into());
        SetBkMode(hdc, TRANSPARENT);

        let font = CreateFontW(
            18, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
            DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, DEFAULT_QUALITY,
            (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
            w!("Microsoft YaHei"),
        );
        let old_font = SelectObject(hdc, font.into());

        // 扁平风格：每行一个白色圆角块，标签左侧、值右侧蓝色
        let rows: [(&str, String); 8] = [
            ("卡片透明度", settings.alpha.to_string()),
            ("双击隐藏", if settings.double_click_hide { "开".into() } else { "关".into() }),
            ("自动分类", if settings.auto_classify { "开".into() } else { "关".into() }),
            ("显示格式", if settings.show_icons { "图标".into() } else { "名称".into() }),
            ("卡片大小", match settings.card_cols { 2 => "大".into(), 4 => "小".into(), _ => "中".into() }),
            ("背景色", format!("#{:06X}", settings.bg_color)),
            ("开机自启", if settings.autostart { "开".into() } else { "关".into() }),
            ("卸载程序", "点击卸载".into()),
        ];
        let row_h = 46;
        let margin = 10;
        let row_brush = CreateSolidBrush(rgb(255, 255, 255));
        for (i, (label, value)) in rows.iter().enumerate() {
            let y = margin + (i as i32) * row_h;
            let row_rect = RECT { left: margin, top: y, right: cw - margin, bottom: y + row_h - 4 };
            FillRect(hdc, &row_rect, row_brush);
            // 标签
            SetTextColor(hdc, rgb(60, 60, 70));
            let lw: Vec<u16> = label.encode_utf16().collect();
            TextOutW(hdc, margin + 12, y + 12, &lw);
            // 值（右侧，蓝色）
            SetTextColor(hdc, rgb(43, 127, 255));
            let vw: Vec<u16> = value.encode_utf16().collect();
            let mut sz = SIZE::default();
            GetTextExtentPoint32W(hdc, &vw, &mut sz);
            TextOutW(hdc, (cw - margin - 12 - sz.cx).max(0), y + 12, &vw);
        }
        DeleteObject(row_brush.into());

        SetTextColor(hdc, rgb(150, 150, 160));
        let hint: Vec<u16> = "点击各行切换设置".encode_utf16().collect();
        TextOutW(hdc, 16, ch - 30, &hint);

        SelectObject(hdc, old_font);
        DeleteObject(font.into());
        EndPaint(hwnd, &ps);
    }
}

fn lparam_x(lp: LPARAM) -> i32 {
    ((lp.0 as u32) & 0xffff) as u16 as i16 as i32
}

fn lparam_y(lp: LPARAM) -> i32 {
    (((lp.0 as u32) >> 16) & 0xffff) as u16 as i16 as i32
}

/// 把鼠标消息的客户区坐标转成屏幕坐标（卡片坐标是屏幕坐标，窗口会跟随卡片移动）
fn lparam_to_screen(hwnd: HWND, lp: LPARAM) -> (i32, i32) {
    let mut pt = POINT { x: lparam_x(lp), y: lparam_y(lp) };
    unsafe { ClientToScreen(hwnd, &mut pt); }
    (pt.x, pt.y)
}

fn get_state(hwnd: HWND) -> Option<&'static mut State> {
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if p == 0 {
        None
    } else {
        unsafe { Some(&mut *(p as *mut State)) }
    }
}
