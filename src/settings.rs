use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Settings {
    pub alpha: u8,               // 卡片透明度 100-255
    pub double_click_hide: bool, // 双击隐藏开关
    pub auto_classify: bool,     // 自动分类开关
    pub show_icons: bool,        // true=显示图标，false=显示名称
    pub card_cols: u32,          // 卡片列数 2/3/4（列越少卡片越大）
    #[serde(default = "default_bg")]
    pub bg_color: u32,           // 卡片背景色 RGB（0xRRGGBB）
}

fn default_bg() -> u32 {
    0x262A36
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            alpha: 200,
            double_click_hide: true,
            auto_classify: true,
            show_icons: false,
            card_cols: 3,
            bg_color: 0x262A36,
        }
    }
}

pub fn load_settings(path: &str) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

pub fn save_settings(s: &Settings, path: &str) {
    if let Ok(j) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(path, j);
    }
}
