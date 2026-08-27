use crate::scanner::{DesktopItem, IconKind};
use serde::{Deserialize, Serialize};

/// 卡片内容显示样式
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum CardStyle {
    Grid, // 图标网格
    List, // 标题列表
}

pub struct Card {
    pub kind: IconKind,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub item_indices: Vec<usize>,
    pub style: CardStyle,
    pub scroll: i32, // 列表滚动偏移（行数）
}

impl Card {
    fn new(kind: IconKind, title: &str) -> Self {
        Self {
            kind,
            title: title.to_string(),
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            item_indices: Vec::new(),
            style: CardStyle::List,
            scroll: 0,
        }
    }
}

/// 自动分类：把桌面项按类型分组到 5 个分区
pub fn classify(items: &[DesktopItem]) -> Vec<Card> {
    let mut cards = vec![
        Card::new(IconKind::App, "应用"),
        Card::new(IconKind::Folder, "文件夹"),
        Card::new(IconKind::Document, "文档"),
        Card::new(IconKind::Image, "图片"),
        Card::new(IconKind::Other, "其他"),
    ];
    for (idx, it) in items.iter().enumerate() {
        if let Some(card) = cards.iter_mut().find(|c| c.kind == it.kind) {
            card.item_indices.push(idx);
        }
    }
    cards
}

/// 网格布局：便签条大小，从桌面顶部开始排列（不铺满全屏）
pub fn layout_cards(cards: &mut [Card], _area_w: i32, _area_h: i32, cols: usize) {
    if cards.is_empty() {
        return;
    }
    let margin = 16;
    let gap = 16;
    let cols = if cols == 0 { 3 } else { cols };
    // 便签条大小（按 cols 档位：列越少卡片越大）
    let (card_w, card_h) = match cols {
        2 => (340, 260),
        4 => (200, 160),
        _ => (260, 200),
    };
    for (i, card) in cards.iter_mut().enumerate() {
        let col = (i % cols) as i32;
        let row = (i / cols) as i32;
        card.x = margin + col * (card_w + gap);
        card.y = margin + row * (card_h + gap);
        card.width = card_w;
        card.height = card_h;
    }
}

/// 命中测试：判断坐标落在哪个卡片（从后往前，上层优先）
pub fn hit_test(cards: &[Card], x: i32, y: i32) -> Option<usize> {
    for (i, card) in cards.iter().enumerate().rev() {
        if x >= card.x && x < card.x + card.width && y >= card.y && y < card.y + card.height {
            return Some(i);
        }
    }
    None
}

/// 命中测试：判断坐标是否落在卡片的右上角 X 按钮
pub fn hit_test_close(cards: &[Card], x: i32, y: i32) -> Option<usize> {
    for (i, card) in cards.iter().enumerate().rev() {
        let cx = card.x + card.width - 16;
        let cy = card.y + 12;
        if x >= cx - 12 && x < cx + 12 && y >= cy - 12 && y < cy + 12 {
            return Some(i);
        }
    }
    None
}

/// 拖拽调整大小的边缘类型
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ResizeKind {
    Right,  // 右边缘：调宽度
    Bottom, // 下边缘：调高度
    Corner, // 右下角：同时调宽高
}

/// 命中测试：判断坐标是否落在卡片边缘（用于调整大小）
pub fn hit_test_resize(cards: &[Card], x: i32, y: i32) -> Option<(usize, ResizeKind)> {
    const EDGE: i32 = 10;
    const CORNER: i32 = 20;
    for (i, card) in cards.iter().enumerate().rev() {
        let right = card.x + card.width;
        let bottom = card.y + card.height;
        // 右下角
        if x >= right - CORNER && x < right && y >= bottom - CORNER && y < bottom {
            return Some((i, ResizeKind::Corner));
        }
        // 右边缘
        if x >= right - EDGE && x < right && y >= card.y && y < bottom {
            return Some((i, ResizeKind::Right));
        }
        // 下边缘
        if y >= bottom - EDGE && y < bottom && x >= card.x && x < right {
            return Some((i, ResizeKind::Bottom));
        }
    }
    None
}

/// 命中测试：判断坐标落在哪个卡片的哪个图标项（返回 (卡片索引, 项列表索引)）
pub fn hit_test_item(cards: &[Card], x: i32, y: i32, show_icons: bool) -> Option<(usize, usize)> {
    for (ci, card) in cards.iter().enumerate().rev() {
        // 内容区（避开右侧按钮区）
        if x < card.x + 8 || x >= card.x + card.width - 28 || y < card.y + 40 {
            continue;
        }
        match card.style {
            CardStyle::Grid => {
                let cell_w = 100;
                let cell_h = 72;
                let cols = ((card.width - 16) / cell_w).max(1);
                let col = (x - card.x - 8) / cell_w;
                let row = (y - card.y - 40) / cell_h;
                let idx = ((row + card.scroll) * cols + col) as usize;
                if idx < card.item_indices.len() {
                    return Some((ci, idx));
                }
            }
            CardStyle::List => {
                let row_h = if show_icons { 24 } else { 20 };
                let mut y_pos = card.y + 40;
                for (ii, _) in card.item_indices.iter().enumerate() {
                    if y >= y_pos && y < y_pos + row_h {
                        return Some((ci, ii));
                    }
                    y_pos += row_h;
                }
            }
        }
    }
    None
}

/// 命中测试：判断坐标是否落在卡片的样式切换按钮（X 按钮左侧，用于网格/列表切换）
pub fn hit_test_style(cards: &[Card], x: i32, y: i32) -> Option<usize> {
    for (i, card) in cards.iter().enumerate().rev() {
        let sx = card.x + card.width - 52; // X 按钮左侧
        let sy = card.y + 12;
        if x >= sx - 12 && x < sx + 12 && y >= sy - 12 && y < sy + 12 {
            return Some(i);
        }
    }
    None
}

/// 命中测试：判断坐标是否落在卡片标题栏（用于拖动移动）
pub fn hit_test_title(cards: &[Card], x: i32, y: i32) -> Option<usize> {
    for (i, card) in cards.iter().enumerate().rev() {
        // 标题栏区域：卡片顶部 36px，避开右侧 X 按钮
        if x >= card.x && x < card.x + card.width - 28 && y >= card.y && y < card.y + 36 {
            return Some(i);
        }
    }
    None
}

/// 检测卡片 idx 的新矩形是否与其他卡片重叠（用于禁止卡片重叠）
pub fn overlaps(cards: &[Card], idx: usize, x: i32, y: i32, w: i32, h: i32) -> bool {
    for (i, card) in cards.iter().enumerate() {
        if i == idx {
            continue;
        }
        if x < card.x + card.width && x + w > card.x && y < card.y + card.height && y + h > card.y {
            return true;
        }
    }
    false
}

#[derive(Serialize, Deserialize)]
pub struct SavedCard {
    pub kind: String,
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
}

/// 保存卡片布局（坐标 + 尺寸）到 JSON
pub fn save_layout(cards: &[Card], path: &str) {
    let saved: Vec<SavedCard> = cards
        .iter()
        .map(|c| SavedCard {
            kind: c.kind.label().to_string(),
            x: c.x,
            y: c.y,
            width: c.width,
            height: c.height,
        })
        .collect();
    if let Ok(json) = serde_json::to_string_pretty(&saved) {
        let _ = std::fs::write(path, json);
    }
}

/// 从 JSON 恢复卡片坐标 + 尺寸（带重叠校验：重叠的坏坐标丢弃，保留默认布局）
pub fn load_layout(cards: &mut [Card], path: &str) {
    if let Ok(content) = std::fs::read_to_string(path) {
        if let Ok(saved) = serde_json::from_str::<Vec<SavedCard>>(&content) {
            // 已恢复的卡片矩形，用于重叠检测
            let mut restored: Vec<(i32, i32, i32, i32)> = Vec::new();
            for sc in saved {
                if let Some(card) = cards.iter_mut().find(|c| c.kind.label() == sc.kind) {
                    let w = if sc.width > 0 { sc.width } else { card.width };
                    let h = if sc.height > 0 { sc.height } else { card.height };
                    // 与已恢复卡片重叠 → 丢弃该坐标（保持默认布局），避免卡片盖住无法选中
                    let mut overlap = false;
                    for (x, y, rw, rh) in &restored {
                        if sc.x < x + rw && sc.x + w > *x && sc.y < y + rh && sc.y + h > *y {
                            overlap = true;
                            break;
                        }
                    }
                    if !overlap {
                        card.x = sc.x;
                        card.y = sc.y;
                        if sc.width > 0 {
                            card.width = sc.width;
                        }
                        if sc.height > 0 {
                            card.height = sc.height;
                        }
                        restored.push((sc.x, sc.y, w, h));
                    }
                }
            }
        }
    }
}

/// 删除指定分区
pub fn remove_card(cards: &mut Vec<Card>, index: usize) {
    if index < cards.len() {
        cards.remove(index);
    }
}

/// 新建空分区
pub fn add_card(cards: &mut Vec<Card>) {
    let n = cards.iter().filter(|c| c.title.starts_with("新分区")).count() + 1;
    cards.push(Card::new(IconKind::Other, &format!("新分区{}", n)));
}
