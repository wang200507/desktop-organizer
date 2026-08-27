use std::path::{Path, PathBuf};
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{FILE_FLAGS_AND_ATTRIBUTES, WIN32_FIND_DATAW};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER, IPersistFile, STGM};
use windows::Win32::UI::Shell::{
    SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON, IShellLinkW,
};
use windows::Win32::UI::WindowsAndMessaging::HICON;

/// CLSID_ShellLink：{00021401-0000-0000-C000-000000000046}
static CLSID_SHELLLINK: windows::core::GUID =
    windows::core::GUID::from_u128(0x00021401_0000_0000_c000_000000000046);

/// 解析 .lnk 快捷方式指向的目标路径（用于取真实图标，去掉快捷方式箭头）
fn resolve_lnk(path: &Path) -> Option<PathBuf> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&CLSID_SHELLLINK, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist: IPersistFile = windows::core::Interface::cast(&link).ok()?;
        let path_str = path.to_string_lossy();
        let path_w: Vec<u16> = path_str.encode_utf16().collect();
        persist.Load(PCWSTR(path_w.as_ptr()), STGM::default()).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut fd = WIN32_FIND_DATAW::default();
        link.GetPath(&mut buf, &mut fd, 0).ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        if len == 0 {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconKind {
    App,
    Folder,
    Document,
    Image,
    Other,
}

impl IconKind {
    pub fn label(&self) -> &'static str {
        match self {
            IconKind::App => "[应用]",
            IconKind::Folder => "[文件夹]",
            IconKind::Document => "[文档]",
            IconKind::Image => "[图片]",
            IconKind::Other => "[其他]",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DesktopItem {
    pub name: String,         // 原始文件名（含 .lnk）
    pub display_name: String, // 去 .lnk/.exe 后缀的显示名
    pub kind: IconKind,
    pub path: PathBuf,
    pub icon: Option<HICON>,  // 图标句柄（SHGetFileInfo 获取）
}

fn classify(ext: &str) -> IconKind {
    match ext.to_lowercase().as_str() {
        "lnk" | "exe" | "appref-ms" => IconKind::App,
        "doc" | "docx" | "pdf" | "txt" | "xls" | "xlsx" | "ppt" | "pptx" | "md" | "csv" => {
            IconKind::Document
        }
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" => IconKind::Image,
        _ => IconKind::Other,
    }
}

/// 去掉应用类的 .lnk/.exe/.appref-ms 后缀
fn strip_suffix(name: &str, kind: IconKind) -> String {
    if kind == IconKind::App {
        for suf in [".lnk", ".exe", ".appref-ms"] {
            let lower = name.to_lowercase();
            if lower.ends_with(suf) {
                return name[..name.len() - suf.len()].to_string();
            }
        }
    }
    name.to_string()
}

fn user_desktop() -> PathBuf {
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| r"C:\Users\admin".into());
    PathBuf::from(home).join("Desktop")
}

fn public_desktop() -> PathBuf {
    let public = std::env::var("PUBLIC").unwrap_or_else(|_| r"C:\Users\Public".into());
    PathBuf::from(public).join("Desktop")
}

/// 用 SHGetFileInfo 获取文件小图标（16x16）；.lnk 先解析目标取真实图标（去快捷方式箭头）
fn load_icon(path: &PathBuf) -> Option<HICON> {
    // .lnk 解析目标路径（失败则回退用 .lnk 自身）
    let icon_path = if path.extension().map_or(false, |e| e.eq_ignore_ascii_case("lnk")) {
        resolve_lnk(path).unwrap_or_else(|| path.clone())
    } else {
        path.clone()
    };
    let path_str = icon_path.to_string_lossy();
    let path_w: Vec<u16> = path_str.encode_utf16().collect();
    let mut sfi = SHFILEINFOW::default();
    unsafe {
        SHGetFileInfoW(
            PCWSTR(path_w.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut sfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON,
        );
    }
    if sfi.hIcon.is_invalid() {
        None
    } else {
        Some(sfi.hIcon)
    }
}

pub fn scan_desktop() -> Vec<DesktopItem> {
    let mut items = Vec::new();

    for base in [user_desktop(), public_desktop()] {
        if !base.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            if name.eq_ignore_ascii_case("desktop.ini") || name.starts_with('.') {
                continue;
            }

            let kind = if path.is_dir() {
                IconKind::Folder
            } else {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                classify(ext)
            };

            let display_name = strip_suffix(&name, kind);
            let icon = load_icon(&path);

            items.push(DesktopItem { name, display_name, kind, path, icon });
        }
    }

    items.sort_by(|a, b| a.kind.label().cmp(b.kind.label()).then(a.name.cmp(&b.name)));
    items
}

pub fn count_by_kind(items: &[DesktopItem]) -> [(IconKind, usize); 5] {
    let mut counts = [
        (IconKind::App, 0),
        (IconKind::Folder, 0),
        (IconKind::Document, 0),
        (IconKind::Image, 0),
        (IconKind::Other, 0),
    ];
    for it in items {
        for entry in counts.iter_mut() {
            if entry.0 == it.kind {
                entry.1 += 1;
            }
        }
    }
    counts
}
