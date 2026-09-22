use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use crate::layout::{Card, CardStyle};
use crate::scanner::DesktopItem;

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

/// 提亮颜色（c 为 0xRRGGBB，f 为亮度倍数）
fn lighten(c: u32, f: f32) -> (u8, u8, u8) {
    (
        (((c >> 16) & 0xff) as f32 * f).min(255.0) as u8,
        (((c >> 8) & 0xff) as f32 * f).min(255.0) as u8,
        ((c & 0xff) as f32 * f).min(255.0) as u8,
    )
}

/// 卡片圆角半径
const CARD_RADIUS: i32 = 12;
/// 标题栏高度（与 layout.rs 命中测试保持一致）
const TITLE_H: i32 = 36;
/// 内容区顶部（标题栏 + 分隔线以下）
const CONTENT_TOP: i32 = 42;
/// 网格单元尺寸（与 layout.rs 命中测试保持一致）
const CELL_W: i32 = 96;
const CELL_H: i32 = 74;

/// 圆角矩形第 y 行的可见 x 区间（含边界逻辑与 GDI 椭圆角近似）
/// 返回 None 表示该行完全在圆角外
fn rounded_row(y: i32, x0: i32, y0: i32, x1: i32, y1: i32, r: i32) -> Option<(i32, i32)> {
    if y < y0 || y >= y1 || x1 <= x0 {
        return None;
    }
    let w = x1 - x0;
    let h = y1 - y0;
    let r = r.clamp(0, (w / 2).min(h / 2));
    if r == 0 {
        return Some((x0, x1));
    }
    let cy_top = y0 + r;
    let cy_bot = y1 - 1 - r;
    let dx = if y < cy_top {
        let dy = (cy_top - y) as f32;
        (r as f32 * r as f32 - dy * dy).max(0.0).sqrt()
    } else if y > cy_bot {
        let dy = (y - cy_bot) as f32;
        (r as f32 * r as f32 - dy * dy).max(0.0).sqrt()
    } else {
        return Some((x0, x1));
    };
    let dxi = dx.ceil() as i32;
    Some((x0 + r - dxi, x1 - r + dxi))
}

pub struct Renderer {
    title_font: HFONT,
    item_font: HFONT,
    grid_font: HFONT,
    mem_dc: Option<HDC>,
    hbitmap: Option<HBITMAP>,
    old_bmp: Option<HGDIOBJ>,
    bits: *mut std::ffi::c_void,
    w: i32,
    h: i32,
    bg_color: u32,
}

impl Renderer {
    pub fn new() -> Self {
        unsafe {
            let title_font = CreateFontW(
                17, 0, 0, 0, FW_BOLD.0 as i32, 0, 0, 0,
                DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY,
                (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
                w!("Microsoft YaHei"),
            );
            let item_font = CreateFontW(
                15, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
                DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY,
                (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
                w!("Microsoft YaHei"),
            );
            let grid_font = CreateFontW(
                12, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
                DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY,
                (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
                w!("Microsoft YaHei"),
            );
            Self {
                title_font,
                item_font,
                grid_font,
                mem_dc: None,
                hbitmap: None,
                old_bmp: None,
                bits: std::ptr::null_mut(),
                w: 0,
                h: 0,
                bg_color: 0x262A36,
            }
        }
    }

    /// 设置卡片背景色（0xRRGGBB）
    pub fn set_bg_color(&mut self, c: u32) {
        self.bg_color = c;
    }

    // 确保内存 DC/DIB 尺寸匹配（复用，避免每帧重建）
    fn ensure_surface(&mut self, w: i32, h: i32) {
        if self.w == w && self.h == h && self.mem_dc.is_some() {
            return;
        }
        unsafe {
            if let (Some(mem_dc), Some(hbitmap), Some(old_bmp)) = (self.mem_dc, self.hbitmap, self.old_bmp) {
                SelectObject(mem_dc, old_bmp);
                DeleteObject(hbitmap.into());
                DeleteDC(mem_dc);
            }
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w;
            bmi.bmiHeader.biHeight = -h;
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = 0;
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let hbitmap = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
                .expect("CreateDIBSection");
            let screen_dc = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen_dc));
            let old_bmp = SelectObject(mem_dc, hbitmap.into());
            ReleaseDC(None, screen_dc);
            self.hbitmap = Some(hbitmap);
            self.mem_dc = Some(mem_dc);
            self.old_bmp = Some(old_bmp);
            self.bits = bits;
            self.w = w;
            self.h = h;
        }
    }

    /// 滚动越界收敛（resize 变小后 scroll 可能超过上限）
    fn clamp_scroll(cards: &mut [Card], show_icons: bool) {
        for card in cards.iter_mut() {
            let content_bottom = card.y + card.height - 8;
            let max_scroll = match card.style {
                CardStyle::Grid => {
                    let cols = ((card.width - 16) / CELL_W).max(1);
                    let rows_visible = ((content_bottom - card.y - CONTENT_TOP) / CELL_H).max(1);
                    let total_rows = ((card.item_indices.len() as i32 + cols - 1) / cols).max(1);
                    (total_rows - rows_visible).max(0)
                }
                CardStyle::List => {
                    let row_h = if show_icons { 26 } else { 22 };
                    let visible_rows = ((content_bottom - card.y - CONTENT_TOP) / row_h).max(1);
                    (card.item_indices.len() as i32 - visible_rows).max(0)
                }
            };
            card.scroll = card.scroll.clamp(0, max_scroll);
        }
    }

    /// 全量渲染：清空整个表面 + 绘制所有卡片（结构性变化用）
    pub fn render(&mut self, hwnd: HWND, cards: &mut [Card], items: &[DesktopItem], visible: bool, alpha: u8, show_icons: bool, selected: Option<(usize, usize)>) {
        unsafe {
            let w = GetSystemMetrics(SM_CXSCREEN);
            let h = GetSystemMetrics(SM_CYSCREEN);
            if w <= 0 || h <= 0 {
                return;
            }
            Self::clamp_scroll(cards, show_icons);
            self.ensure_surface(w, h);
            let mem_dc = self.mem_dc.unwrap();

            // 清空 DIB
            std::ptr::write_bytes(self.bits as *mut u8, 0, (w * h * 4) as usize);
            SetBkMode(mem_dc, TRANSPARENT);

            if !visible {
                // 桌面已隐藏：窗口全屏，中央画"恢复提示"卡片（双击恢复）
                self.draw_hidden_hint(mem_dc, w, h);
                let hint_w = 300;
                let hint_h = 68;
                let cx = (w - hint_w) / 2;
                let cy = (h - hint_h) / 2;
                self.blend_cards(&[(cx, cy, hint_w, hint_h)], &[(0, 0, w, h)], 200, 16);
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let src_pt = POINT { x: 0, y: 0 };
                let dst_pt = POINT { x: 0, y: 0 };
                let size = SIZE { cx: w, cy: h };
                let _ = UpdateLayeredWindow(
                    hwnd,
                    None,
                    Some(&dst_pt),
                    Some(&size),
                    Some(mem_dc),
                    Some(&src_pt),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
                return;
            }

            for (ci, card) in cards.iter().enumerate() {
                self.draw_card(mem_dc, card, items, ci, show_icons, selected);
            }
            let rects: Vec<(i32, i32, i32, i32)> = cards
                .iter()
                .map(|c| (c.x, c.y, c.width, c.height))
                .collect();
            self.blend_cards(&rects, &[(0, 0, w, h)], alpha, CARD_RADIUS);

            // UpdateLayeredWindow 直接携带窗口新位置/尺寸（无需额外 SetWindowPos，
            // 少一次窗口移动 + 一次合成，消除拖拽时的消息风暴）
            let bbox = compute_bbox(cards);
            let (ox, oy, cw, ch) = match bbox {
                Some((x0, y0, x1, y1)) => {
                    let x0 = x0.max(0);
                    let y0 = y0.max(0);
                    let x1 = x1.min(w);
                    let y1 = y1.min(h);
                    (x0, y0, (x1 - x0).max(1), (y1 - y0).max(1))
                }
                None => (0, 0, 1, 1),
            };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let src_pt = POINT { x: ox, y: oy };
            let size = SIZE { cx: cw, cy: ch };
            let dst_pt = POINT { x: ox, y: oy };
            let _ = UpdateLayeredWindow(
                hwnd,
                None,
                Some(&dst_pt),
                Some(&size),
                Some(mem_dc),
                Some(&src_pt),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }

    /// 局部渲染：只清空/重绘 damage 区域（拖拽、滚动、选中切换用，避免全屏重绘卡顿）
    /// damage 为屏幕坐标矩形 (x0, y0, x1, y1)
    pub fn render_damage(&mut self, hwnd: HWND, cards: &mut [Card], items: &[DesktopItem], visible: bool, alpha: u8, show_icons: bool, selected: Option<(usize, usize)>, damage: &[(i32, i32, i32, i32)]) {
        if !visible {
            // 隐藏态只有一张提示卡，直接走全量
            self.render(hwnd, cards, items, visible, alpha, show_icons, selected);
            return;
        }
        unsafe {
            let w = GetSystemMetrics(SM_CXSCREEN);
            let h = GetSystemMetrics(SM_CYSCREEN);
            if w <= 0 || h <= 0 {
                return;
            }
            // 裁剪到屏幕内的有效区域
            let rects: Vec<(i32, i32, i32, i32)> = damage
                .iter()
                .map(|&(x0, y0, x1, y1)| {
                    (
                        x0.max(0),
                        y0.max(0),
                        x1.min(w),
                        y1.min(h),
                    )
                })
                .filter(|&(x0, y0, x1, y1)| x1 > x0 && y1 > y0)
                .collect();
            if rects.is_empty() {
                return;
            }
            Self::clamp_scroll(cards, show_icons);
            self.ensure_surface(w, h);
            let mem_dc = self.mem_dc.unwrap();
            let data = self.bits as *mut u8;

            // 1) 只清空 damage 区域（避免整屏 8MB+ memset）
            for &(x0, y0, x1, y1) in &rects {
                for y in y0..y1 {
                    let off = ((y * w + x0) * 4) as usize;
                    std::ptr::write_bytes(data.add(off), 0, ((x1 - x0) * 4) as usize);
                }
            }

            // 2) GDI 裁剪到 damage，只重绘相交卡片（未相交卡片像素保持上一帧）
            let clip = CreateRectRgn(rects[0].0, rects[0].1, rects[0].2, rects[0].3);
            if rects.len() > 1 {
                let second = CreateRectRgn(rects[1].0, rects[1].1, rects[1].2, rects[1].3);
                let _ = CombineRgn(Some(clip), Some(clip), Some(second), RGN_OR);
                let _ = DeleteObject(second.into());
            }
            SelectClipRgn(mem_dc, Some(clip));
            SetBkMode(mem_dc, TRANSPARENT);
            for (ci, card) in cards.iter().enumerate() {
                let intersects = rects.iter().any(|&(x0, y0, x1, y1)| {
                    card.x < x1 && card.x + card.width > x0 && card.y < y1 && card.y + card.height > y0
                });
                if intersects {
                    self.draw_card(mem_dc, card, items, ci, show_icons, selected);
                }
            }
            SelectClipRgn(mem_dc, None);
            let _ = DeleteObject(clip.into());

            // 3) damage 内写入 per-pixel alpha（圆角 + 扁平描边）
            let card_rects: Vec<(i32, i32, i32, i32)> = cards
                .iter()
                .map(|c| (c.x, c.y, c.width, c.height))
                .collect();
            self.blend_cards(&card_rects, &rects, alpha, CARD_RADIUS);

            // 4) 提交（窗口范围 = 全部卡片包围盒，与全量渲染一致）
            let bbox = compute_bbox(cards);
            let (ox, oy, cw, ch) = match bbox {
                Some((x0, y0, x1, y1)) => {
                    let x0 = x0.max(0);
                    let y0 = y0.max(0);
                    let x1 = x1.min(w);
                    let y1 = y1.min(h);
                    (x0, y0, (x1 - x0).max(1), (y1 - y0).max(1))
                }
                None => (0, 0, 1, 1),
            };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let src_pt = POINT { x: ox, y: oy };
            let size = SIZE { cx: cw, cy: ch };
            let dst_pt = POINT { x: ox, y: oy };
            let _ = UpdateLayeredWindow(
                hwnd,
                None,
                Some(&dst_pt),
                Some(&size),
                Some(mem_dc),
                Some(&src_pt),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }

    /// 隐藏态提示卡（居中，扁平圆角）
    fn draw_hidden_hint(&self, mem_dc: HDC, w: i32, h: i32) {
        unsafe {
            let hint_w = 300;
            let hint_h = 68;
            let cx = (w - hint_w) / 2;
            let cy = (h - hint_h) / 2;
            let (r, g, b) = lighten(self.bg_color, 1.0);
            let brush = CreateSolidBrush(rgb(r, g, b));
            let rect = RECT { left: cx, top: cy, right: cx + hint_w, bottom: cy + hint_h };
            FillRect(mem_dc, &rect, brush);
            DeleteObject(brush.into());

            // 标题文字居中
            let old_font = SelectObject(mem_dc, self.title_font.into());
            SetTextColor(mem_dc, rgb(238, 240, 246));
            let hint: Vec<u16> = "桌面已隐藏 · 双击这里恢复".encode_utf16().collect();
            let mut sz = SIZE::default();
            GetTextExtentPoint32W(mem_dc, &hint, &mut sz);
            TextOutW(mem_dc, cx + (hint_w - sz.cx) / 2, cy + 24, &hint);
            SelectObject(mem_dc, old_font);
        }
    }

    /// 绘制单张卡片（扁平化：左对齐标题 + 强调圆点 + 分隔线 + 右上角按钮区）
    fn draw_card(&self, mem_dc: HDC, card: &Card, items: &[DesktopItem], ci: usize, show_icons: bool, selected: Option<(usize, usize)>) {
        unsafe {
            let x = card.x;
            let y = card.y;
            let right = card.x + card.width;
            let bottom = card.y + card.height;

            // 背景
            let (br, bg, bb) = lighten(self.bg_color, 1.0);
            let bg_brush = CreateSolidBrush(rgb(br, bg, bb));
            let rect = RECT { left: x, top: y, right, bottom };
            FillRect(mem_dc, &rect, bg_brush);
            DeleteObject(bg_brush.into());

            // 标题栏分隔线（扁平细节）
            let (lr, lg, lb) = lighten(self.bg_color, 1.25);
            let line_brush = CreateSolidBrush(rgb(lr, lg, lb));
            let line_rect = RECT { left: x + 12, top: y + TITLE_H - 2, right: x + card.width - 12, bottom: y + TITLE_H - 1 };
            FillRect(mem_dc, &line_rect, line_brush);
            DeleteObject(line_brush.into());

            // 左侧强调圆点（NULL_PEN 避免描边）
            let dot_brush = CreateSolidBrush(rgb(86, 156, 214));
            let old_pen = SelectObject(mem_dc, GetStockObject(NULL_PEN));
            let _ = Ellipse(mem_dc, x + 14, y + 13, x + 22, y + 21);
            SelectObject(mem_dc, old_pen);
            DeleteObject(dot_brush.into());

            // 标题（左对齐）
            let old_font = SelectObject(mem_dc, self.title_font.into());
            SetTextColor(mem_dc, rgb(238, 240, 246));
            let title: Vec<u16> = card.title.encode_utf16().collect();
            TextOutW(mem_dc, x + 28, y + 10, &title);

            // 数量徽标（小号灰字，右对齐到按钮区左侧）
            let g_old_font = SelectObject(mem_dc, self.grid_font.into());
            SetTextColor(mem_dc, rgb(150, 158, 175));
            let cnt = card.item_indices.len().to_string();
            let cw: Vec<u16> = cnt.encode_utf16().collect();
            let mut sz = SIZE::default();
            GetTextExtentPoint32W(mem_dc, &cw, &mut sz);
            TextOutW(mem_dc, (right - 82 - sz.cx).max(x + 30), y + 14, &cw);
            SelectObject(mem_dc, g_old_font);

            // 右上角 X 按钮
            SetTextColor(mem_dc, rgb(196, 132, 134));
            let x_w: Vec<u16> = "✕".encode_utf16().collect();
            TextOutW(mem_dc, right - 26, y + 7, &x_w);

            // 样式切换按钮（X 左侧）：List 显示 ▦（点它切网格），Grid 显示 ≡（点它切列表）
            SetTextColor(mem_dc, rgb(152, 164, 188));
            let style_icon: Vec<u16> = match card.style {
                CardStyle::Grid => "≡".encode_utf16().collect(),
                CardStyle::List => "▦".encode_utf16().collect(),
            };
            TextOutW(mem_dc, right - 54, y + 7, &style_icon);
            SelectObject(mem_dc, old_font);

            // 内容区
            let content_bottom = bottom - 8;
            match card.style {
                CardStyle::Grid => {
                    let cols = ((card.width - 16) / CELL_W).max(1);
                    let start_x = x + 8;
                    let start_y = y + CONTENT_TOP;
                    let rows_visible = ((content_bottom - start_y) / CELL_H).max(1);
                    let total = card.item_indices.len() as i32;
                    let total_rows = ((total + cols - 1) / cols).max(1);
                    let max_scroll = (total_rows - rows_visible).max(0);
                    let scroll = card.scroll.clamp(0, max_scroll);
                    for (ii, &idx) in card.item_indices.iter().enumerate() {
                        let i = ii as i32;
                        let row_global = i / cols;
                        if row_global < scroll {
                            continue;
                        }
                        let row = row_global - scroll;
                        let col = i % cols;
                        let gx = start_x + col * CELL_W;
                        let gy = start_y + row * CELL_H;
                        if gy + CELL_H > content_bottom {
                            break;
                        }
                        if let Some(it) = items.get(idx) {
                            if selected == Some((ci, ii)) {
                                let sel_brush = CreateSolidBrush(rgb(58, 86, 132));
                                let sel_rect = RECT { left: gx, top: gy, right: gx + CELL_W - 4, bottom: gy + CELL_H - 2 };
                                FillRect(mem_dc, &sel_rect, sel_brush);
                                DeleteObject(sel_brush.into());
                            }
                            if let Some(icon) = it.icon {
                                let _ = DrawIconEx(mem_dc, gx + (CELL_W - 32) / 2, gy + 6, icon, 32, 32, 0, None, DI_NORMAL);
                            }
                            let g_old = SelectObject(mem_dc, self.grid_font.into());
                            SetTextColor(mem_dc, rgb(218, 222, 232));
                            // 名字截断（超格宽显示 …），图标下方居中
                            let short: String = it.display_name.chars().take(6).collect();
                            let mut t: Vec<u16> = short.encode_utf16().collect();
                            if it.display_name.chars().count() > 6 {
                                t.extend("…".encode_utf16());
                            }
                            let mut tsz = SIZE::default();
                            GetTextExtentPoint32W(mem_dc, &t, &mut tsz);
                            TextOutW(mem_dc, gx + ((CELL_W - 4 - tsz.cx) / 2).max(2), gy + 42, &t);
                            SelectObject(mem_dc, g_old);
                        }
                    }
                    // 滚动条（扁平：仅滑块）
                    if max_scroll > 0 {
                        let sb_x = right - 10;
                        let sb_top = y + CONTENT_TOP;
                        let sb_h = (content_bottom - sb_top).max(20);
                        let thumb_h = ((rows_visible as f32 / total_rows as f32) * sb_h as f32).max(16.0) as i32;
                        let thumb_y = sb_top + ((scroll as f32 / max_scroll as f32) * (sb_h - thumb_h) as f32) as i32;
                        let thumb_brush = CreateSolidBrush(rgb(130, 140, 165));
                        let th_rect = RECT { left: sb_x, top: thumb_y, right: sb_x + 5, bottom: thumb_y + thumb_h };
                        FillRect(mem_dc, &th_rect, thumb_brush);
                        DeleteObject(thumb_brush.into());
                    }
                }
                CardStyle::List => {
                    let row_h = if show_icons { 26 } else { 22 };
                    let visible_rows = ((content_bottom - y - CONTENT_TOP) / row_h).max(1);
                    let total = card.item_indices.len() as i32;
                    let max_scroll = (total - visible_rows).max(0);
                    let scroll = card.scroll.clamp(0, max_scroll);
                    let mut py = y + CONTENT_TOP;
                    for (ii, &idx) in card.item_indices.iter().enumerate() {
                        if (ii as i32) < scroll {
                            continue;
                        }
                        if py > content_bottom {
                            break;
                        }
                        if let Some(it) = items.get(idx) {
                            if selected == Some((ci, ii)) {
                                let sel_brush = CreateSolidBrush(rgb(58, 86, 132));
                                let sel_rect = RECT { left: x + 4, top: py - 1, right: right - 10, bottom: py + row_h - 1 };
                                FillRect(mem_dc, &sel_rect, sel_brush);
                                DeleteObject(sel_brush.into());
                            }
                            let l_old = SelectObject(mem_dc, self.item_font.into());
                            SetTextColor(mem_dc, rgb(218, 222, 232));
                            if show_icons {
                                if let Some(icon) = it.icon {
                                    let _ = DrawIconEx(mem_dc, x + 12, py + 3, icon, 16, 16, 0, None, DI_NORMAL);
                                }
                                let line_w: Vec<u16> = it.display_name.encode_utf16().collect();
                                TextOutW(mem_dc, x + 34, py + 3, &line_w);
                            } else {
                                let line_w: Vec<u16> = it.display_name.encode_utf16().collect();
                                TextOutW(mem_dc, x + 12, py + 3, &line_w);
                            }
                            SelectObject(mem_dc, l_old);
                        }
                        py += row_h;
                    }
                    if max_scroll > 0 {
                        let sb_x = right - 10;
                        let sb_top = y + CONTENT_TOP;
                        let sb_h = (content_bottom - sb_top).max(20);
                        let thumb_h = ((visible_rows as f32 / total as f32) * sb_h as f32).max(16.0) as i32;
                        let thumb_y = sb_top + ((scroll as f32 / max_scroll as f32) * (sb_h - thumb_h) as f32) as i32;
                        let thumb_brush = CreateSolidBrush(rgb(130, 140, 165));
                        let th_rect = RECT { left: sb_x, top: thumb_y, right: sb_x + 5, bottom: thumb_y + thumb_h };
                        FillRect(mem_dc, &th_rect, thumb_brush);
                        DeleteObject(thumb_brush.into());
                    }
                }
            }
        }
    }

    /// 在 damage 区域内为卡片矩形 (x, y, w, h) 写入 per-pixel alpha：圆角裁形 + 1px 亮色描边（扁平）
    /// 数组顺序即层级：后画的卡片覆盖先画的（与绘制顺序一致）
    fn blend_cards(&self, rects: &[(i32, i32, i32, i32)], damage: &[(i32, i32, i32, i32)], alpha: u8, radius: i32) {
        let w = self.w;
        let h = self.h;
        if w <= 0 || h <= 0 || self.bits.is_null() {
            return;
        }
        let data = self.bits as *mut u8;
        let (bdr, bdg, bdb) = lighten(self.bg_color, 1.5);
        let border_alpha = alpha.saturating_add(45);
        for &(rx, ry, rw, rh) in rects {
            let cx0 = rx.max(0);
            let cy0 = ry.max(0);
            let cx1 = (rx + rw).min(w);
            let cy1 = (ry + rh).min(h);
            if cx1 <= cx0 || cy1 <= cy0 {
                continue;
            }
            for &(dx0, dy0, dx1, dy1) in damage {
                let rx0 = cx0.max(dx0);
                let ry0 = cy0.max(dy0);
                let rx1 = cx1.min(dx1);
                let ry1 = cy1.min(dy1);
                if rx1 <= rx0 || ry1 <= ry0 {
                    continue;
                }
                for y in ry0..ry1 {
                    let Some((xs, xe)) = rounded_row(y, cx0, cy0, cx1, cy1, radius) else { continue };
                    let xs = xs.max(rx0);
                    let xe = xe.min(rx1);
                    if xe <= xs {
                        continue;
                    }
                    // 圆角带（顶/底各 radius 行）整行视作描边；中部行首尾 1px 描边
                    let corner_band = y < cy0 + radius || y > cy1 - 1 - radius;
                    let row = (y * w) as usize;
                    if corner_band {
                        for x in xs..xe {
                            let off = (row + x as usize) * 4;
                            unsafe {
                                *data.add(off) = bdb;
                                *data.add(off + 1) = bdg;
                                *data.add(off + 2) = bdr;
                                *data.add(off + 3) = border_alpha;
                            }
                        }
                    } else {
                        for x in xs..xe {
                            let off = (row + x as usize) * 4;
                            let a = if x == xs || x == xe - 1 { border_alpha } else { alpha };
                            unsafe { *data.add(off + 3) = a; }
                        }
                    }
                }
            }
        }
    }
}

/// 计算所有卡片的包围盒
fn compute_bbox(cards: &[Card]) -> Option<(i32, i32, i32, i32)> {
    if cards.is_empty() {
        return None;
    }
    let mut x0 = i32::MAX;
    let mut y0 = i32::MAX;
    let mut x1 = i32::MIN;
    let mut y1 = i32::MIN;
    for card in cards {
        x0 = x0.min(card.x);
        y0 = y0.min(card.y);
        x1 = x1.max(card.x + card.width);
        y1 = y1.max(card.y + card.height);
    }
    Some((x0, y0, x1, y1))
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            if !self.title_font.is_invalid() {
                DeleteObject(self.title_font.into());
            }
            if !self.item_font.is_invalid() {
                DeleteObject(self.item_font.into());
            }
            if !self.grid_font.is_invalid() {
                DeleteObject(self.grid_font.into());
            }
            if let (Some(mem_dc), Some(hbitmap), Some(old_bmp)) = (self.mem_dc, self.hbitmap, self.old_bmp) {
                SelectObject(mem_dc, old_bmp);
                DeleteObject(hbitmap.into());
                DeleteDC(mem_dc);
            }
        }
    }
}
