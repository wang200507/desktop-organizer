use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use crate::layout::{Card, CardStyle};
use crate::layout::{card_scale, title_h, content_top, cell_w, cell_h, row_h};
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

/// 卡片圆角半径（基准，随卡片缩放）
const CARD_RADIUS: i32 = 12;

/// 创建指定字号的 Microsoft YaHei 字体（CLEARTYPE）
fn make_font(height: i32, bold: bool) -> HFONT {
    unsafe {
        CreateFontW(
            height, 0, 0, 0, if bold { FW_BOLD.0 as i32 } else { FW_NORMAL.0 as i32 }, 0, 0, 0,
            DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY,
            (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
            w!("Microsoft YaHei"),
        )
    }
}

/// 圆角矩形带符号距离（SDF）：返回 <0 表示在内部，>0 在外部，0 在边界上。
/// 输入为像素采样中心坐标与矩形 [x0,x1)×[y0,y1)，圆角半径 r。
fn rounded_sd(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let hw = (x1 - x0) * 0.5;
    let hh = (y1 - y0) * 0.5;
    let cxm = (x0 + x1) * 0.5;
    let cym = (y0 + y1) * 0.5;
    let ax = hw - r;
    let ay = hh - r;
    let qx = px - cxm;
    let qy = py - cym;
    let bx = qx.abs() - ax;
    let by = qy.abs() - ay;
    let inner = bx.max(by).max(0.0).min(0.0);
    let outer = (bx.max(0.0) * bx.max(0.0) + by.max(0.0) * by.max(0.0)).sqrt();
    inner + outer - r
}

/// 圆角矩形第 y 行的粗略可见 x 区间（仅用于裁剪外层循环，含扫描范围放宽 1px 以容纳抗锯齿）
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
    let dxi = dx.ceil() as i32 + 1; // +1 为 AA 留余量
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
        let title_font = make_font(17, true);
        let item_font = make_font(15, false);
        let grid_font = make_font(12, false);
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

    /// 滚动越界收敛（resize 变小后 scroll 可能超过上限）——按卡片缩放后的几何计算
    fn clamp_scroll(cards: &mut [Card], show_icons: bool) {
        for card in cards.iter_mut() {
            let content_bottom = card.y + card.height - 8;
            let max_scroll = match card.style {
                CardStyle::Grid => {
                    let cw = cell_w(card.width).max(1);
                    let ch = cell_h(card.width).max(1);
                    let cols = ((card.width - 16) / cw).max(1);
                    let rows_visible = ((content_bottom - card.y - content_top(card.width)) / ch).max(1);
                    let total_rows = ((card.item_indices.len() as i32 + cols - 1) / cols).max(1);
                    (total_rows - rows_visible).max(0)
                }
                CardStyle::List => {
                    let rh = row_h(card.width, show_icons).max(1);
                    let visible_rows = ((content_bottom - card.y - content_top(card.width)) / rh).max(1);
                    (card.item_indices.len() as i32 - visible_rows).max(0)
                }
            };
            card.scroll = card.scroll.clamp(0, max_scroll);
        }
    }

    /// 全量渲染：清空整个表面 + 绘制所有卡片（结构性变化与卡片拖动用）
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
                    hwnd, None, Some(&dst_pt), Some(&size), Some(mem_dc), Some(&src_pt),
                    COLORREF(0), Some(&blend), ULW_ALPHA,
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
                hwnd, None, Some(&dst_pt), Some(&size), Some(mem_dc), Some(&src_pt),
                COLORREF(0), Some(&blend), ULW_ALPHA,
            );
        }
    }

    /// 局部渲染：只清空/重绘 damage 区域（滚动、选中切换用，避免全屏重绘卡顿）
    pub fn render_damage(&mut self, hwnd: HWND, cards: &mut [Card], items: &[DesktopItem], visible: bool, alpha: u8, show_icons: bool, selected: Option<(usize, usize)>, damage: &[(i32, i32, i32, i32)]) {
        if !visible {
            self.render(hwnd, cards, items, visible, alpha, show_icons, selected);
            return;
        }
        unsafe {
            let w = GetSystemMetrics(SM_CXSCREEN);
            let h = GetSystemMetrics(SM_CYSCREEN);
            if w <= 0 || h <= 0 {
                return;
            }
            let rects: Vec<(i32, i32, i32, i32)> = damage
                .iter()
                .map(|&(x0, y0, x1, y1)| (x0.max(0), y0.max(0), x1.min(w), y1.min(h)))
                .filter(|&(x0, y0, x1, y1)| x1 > x0 && y1 > y0)
                .collect();
            if rects.is_empty() {
                return;
            }
            Self::clamp_scroll(cards, show_icons);
            self.ensure_surface(w, h);
            let mem_dc = self.mem_dc.unwrap();
            let data = self.bits as *mut u8;

            // 1) 只清空 damage 区域
            for &(x0, y0, x1, y1) in &rects {
                for y in y0..y1 {
                    let off = ((y * w + x0) * 4) as usize;
                    std::ptr::write_bytes(data.add(off), 0, ((x1 - x0) * 4) as usize);
                }
            }

            // 2) GDI 裁剪到 damage，只重绘相交卡片
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

            // 3) damage 内写入 per-pixel alpha（SDF 抗锯齿圆角 + 1px 亮色描边）
            let card_rects: Vec<(i32, i32, i32, i32)> = cards
                .iter()
                .map(|c| (c.x, c.y, c.width, c.height))
                .collect();
            self.blend_cards(&card_rects, &rects, alpha, CARD_RADIUS);

            // 4) 提交（窗口范围 = 全部卡片包围盒）
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
                hwnd, None, Some(&dst_pt), Some(&size), Some(mem_dc), Some(&src_pt),
                COLORREF(0), Some(&blend), ULW_ALPHA,
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

            let old_font = SelectObject(mem_dc, self.title_font.into());
            SetTextColor(mem_dc, rgb(238, 240, 246));
            let hint: Vec<u16> = "桌面已隐藏 · 双击这里恢复".encode_utf16().collect();
            let mut sz = SIZE::default();
            GetTextExtentPoint32W(mem_dc, &hint, &mut sz);
            TextOutW(mem_dc, cx + (hint_w - sz.cx) / 2, cy + 24, &hint);
            SelectObject(mem_dc, old_font);
        }
    }

    /// 绘制单张卡片：随卡片宽度等比缩放；
    /// 标题栏有独立背景 + 分隔线 + 强调圆点 + 左对齐标题 + 数量徽标 + 右上角按钮区
    fn draw_card(&self, mem_dc: HDC, card: &Card, items: &[DesktopItem], ci: usize, show_icons: bool, selected: Option<(usize, usize)>) {
        unsafe {
            let s = card_scale(card.width);
            let x = card.x;
            let y = card.y;
            let right = card.x + card.width;
            let bottom = card.y + card.height;
            let th = title_h(card.width).max(24);
            let ct = content_top(card.width);

            // 背景
            let (br, bg, bb) = lighten(self.bg_color, 1.0);
            let bg_brush = CreateSolidBrush(rgb(br, bg, bb));
            let rect = RECT { left: x, top: y, right, bottom };
            FillRect(mem_dc, &rect, bg_brush);
            DeleteObject(bg_brush.into());

            // 标题栏独立背景（略亮一档，区分于内容区）
            let (hr, hg, hb) = lighten(self.bg_color, 1.08);
            let hdr_brush = CreateSolidBrush(rgb(hr, hg, hb));
            let hdr_rect = RECT { left: x, top: y, right, bottom: y + th };
            FillRect(mem_dc, &hdr_rect, hdr_brush);
            DeleteObject(hdr_brush.into());

            // 标题栏底色带（顶部高光）、底部分隔线
            let (lr, lg, lb) = lighten(self.bg_color, 1.35);
            let line_brush = CreateSolidBrush(rgb(lr, lg, lb));
            let lx = (12.0 * s) as i32;
            let line_rect = RECT { left: x + lx, top: y + th - 2, right: x + card.width - lx, bottom: y + th - 1 };
            FillRect(mem_dc, &line_rect, line_brush);
            DeleteObject(line_brush.into());

            // 标题栏字体（缩放）
            let title_pt = ((17.0 * s).max(11.0)) as i32;
            let grid_pt = ((12.0 * s).max(9.0)) as i32;
            let item_pt = ((15.0 * s).max(10.0)) as i32;
            let title_font = make_font(title_pt, true);
            let grid_font = make_font(grid_pt, false);
            let item_font = make_font(item_pt, false);

            // 左侧强调圆点
            let (dr, dg, db) = (86, 156, 214);
            let dot_brush = CreateSolidBrush(rgb(dr, dg, db));
            let dot_old_pen = SelectObject(mem_dc, GetStockObject(NULL_PEN));
            let _ = Ellipse(
                mem_dc,
                x + (14.0 * s) as i32, y + (13.0 * s) as i32,
                x + (22.0 * s) as i32, y + (21.0 * s) as i32,
            );
            SelectObject(mem_dc, dot_old_pen);
            DeleteObject(dot_brush.into());

            // 标题（左对齐，随卡片缩放字号）
            let old_font = SelectObject(mem_dc, title_font.into());
            SetTextColor(mem_dc, rgb(238, 240, 246));
            let title: Vec<u16> = card.title.encode_utf16().collect();
            TextOutW(mem_dc, x + (28.0 * s) as i32, y + (10.0 * s) as i32, &title);

            // 数量徽标（小号灰字，右对齐到按钮区左侧）
            let g_old = SelectObject(mem_dc, grid_font.into());
            SetTextColor(mem_dc, rgb(150, 158, 175));
            let cnt = card.item_indices.len().to_string();
            let cw16: Vec<u16> = cnt.encode_utf16().collect();
            let mut sz = SIZE::default();
            GetTextExtentPoint32W(mem_dc, &cw16, &mut sz);
            TextOutW(mem_dc, (right - (78.0 * s) as i32 - sz.cx).max(x + (30.0 * s) as i32), y + (14.0 * s) as i32, &cw16);
            SelectObject(mem_dc, g_old);

            // 右上角 X 按钮
            SetTextColor(mem_dc, rgb(196, 132, 134));
            let x_w: Vec<u16> = "✕".encode_utf16().collect();
            TextOutW(mem_dc, right - (26.0 * s) as i32, y + (7.0 * s) as i32, &x_w);

            // 样式切换按钮（X 左侧）：List 显示 ▦（点它切网格），Grid 显示 ≡（点它切列表）
            SetTextColor(mem_dc, rgb(152, 164, 188));
            let style_icon: Vec<u16> = match card.style {
                CardStyle::Grid => "≡".encode_utf16().collect(),
                CardStyle::List => "▦".encode_utf16().collect(),
            };
            TextOutW(mem_dc, right - (54.0 * s) as i32, y + (7.0 * s) as i32, &style_icon);
            SelectObject(mem_dc, old_font);

            // 内容区（随卡片缩放）
            let content_bottom = bottom - 8;
            let cw = cell_w(card.width).max(1);
            let ch = cell_h(card.width).max(1);
            match card.style {
                CardStyle::Grid => {
                    let cols = ((card.width - 16) / cw).max(1);
                    let start_x = x + (8.0 * s) as i32;
                    let start_y = y + ct;
                    let rows_visible = ((content_bottom - start_y) / ch).max(1);
                    let total = card.item_indices.len() as i32;
                    let total_rows = ((total + cols - 1) / cols).max(1);
                    let max_scroll = (total_rows - rows_visible).max(0);
                    let scroll = card.scroll.clamp(0, max_scroll);
                    // 图标尺寸随卡片缩放（上限受单元高约束，给名字留空间）
                    let icon_sz = ((32.0 * s).max(20.0)).min((ch as f32 * 0.55) as f32) as i32;
                    let name_gap = (6.0 * s) as i32;
                    for (ii, &idx) in card.item_indices.iter().enumerate() {
                        let i = ii as i32;
                        let row_global = i / cols;
                        if row_global < scroll {
                            continue;
                        }
                        let row = row_global - scroll;
                        let col = i % cols;
                        let gx = start_x + col * cw;
                        let gy = start_y + row * ch;
                        if gy + ch > content_bottom {
                            break;
                        }
                        if let Some(it) = items.get(idx) {
                            if selected == Some((ci, ii)) {
                                let sel_brush = CreateSolidBrush(rgb(58, 86, 132));
                                let sel_rect = RECT { left: gx, top: gy, right: gx + cw - 4, bottom: gy + ch - 2 };
                                FillRect(mem_dc, &sel_rect, sel_brush);
                                DeleteObject(sel_brush.into());
                            }
                            // 图标（无图标时用首字符占位，避免"空白"）
                            let icon_x = gx + (cw - icon_sz) / 2;
                            let icon_y = gy + (6.0 * s) as i32;
                            if let Some(icon) = it.icon {
                                let _ = DrawIconEx(mem_dc, icon_x, icon_y, icon, icon_sz, icon_sz, 0, None, DI_NORMAL);
                            } else {
                                draw_fallback_badge(mem_dc, icon_x, icon_y, icon_sz, it, x + y);
                            }
                            // 名字截断（超格宽显示 …），图标下方居中
                            let g_old = SelectObject(mem_dc, grid_font.into());
                            SetTextColor(mem_dc, rgb(218, 222, 232));
                            let short: String = it.display_name.chars().take(6).collect();
                            let mut t: Vec<u16> = short.encode_utf16().collect();
                            if it.display_name.chars().count() > 6 {
                                t.extend("…".encode_utf16());
                            }
                            let mut tsz = SIZE::default();
                            GetTextExtentPoint32W(mem_dc, &t, &mut tsz);
                            TextOutW(mem_dc, gx + ((cw - 4 - tsz.cx) / 2).max(2), gy + icon_sz + name_gap, &t);
                            SelectObject(mem_dc, g_old);
                        }
                    }
                    // 滚动条（扁平：仅滑块）
                    if max_scroll > 0 {
                        let sb_x = right - (10.0 * s) as i32;
                        let sb_top = y + ct;
                        let sb_h = (content_bottom - sb_top).max(20);
                        let thumb_h = ((rows_visible as f32 / total_rows as f32) * sb_h as f32).max(16.0) as i32;
                        let thumb_y = sb_top + ((scroll as f32 / max_scroll as f32) * (sb_h - thumb_h) as f32) as i32;
                        let thumb_brush = CreateSolidBrush(rgb(130, 140, 165));
                        let thw = ((5.0 * s) as i32).max(3);
                        let th_rect = RECT { left: sb_x, top: thumb_y, right: sb_x + thw, bottom: thumb_y + thumb_h };
                        FillRect(mem_dc, &th_rect, thumb_brush);
                        DeleteObject(thumb_brush.into());
                    }
                }
                CardStyle::List => {
                    let rh = row_h(card.width, show_icons).max(1);
                    let visible_rows = ((content_bottom - y - ct) / rh).max(1);
                    let total = card.item_indices.len() as i32;
                    let max_scroll = (total - visible_rows).max(0);
                    let scroll = card.scroll.clamp(0, max_scroll);
                    let icon_sz = (16.0 * s) as i32;
                    let mut py = y + ct;
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
                                let sel_rect = RECT { left: x + 4, top: py - 1, right: right - (10.0 * s) as i32, bottom: py + rh - 1 };
                                FillRect(mem_dc, &sel_rect, sel_brush);
                                DeleteObject(sel_brush.into());
                            }
                            let l_old = SelectObject(mem_dc, item_font.into());
                            SetTextColor(mem_dc, rgb(218, 222, 232));
                            if show_icons {
                                let iy = py + ((rh - icon_sz) / 2);
                                if let Some(icon) = it.icon {
                                    let _ = DrawIconEx(mem_dc, x + (12.0 * s) as i32, iy, icon, icon_sz, icon_sz, 0, None, DI_NORMAL);
                                } else {
                                    draw_fallback_badge(mem_dc, x + (12.0 * s) as i32, iy, icon_sz, it, x + y);
                                }
                                let line_w: Vec<u16> = it.display_name.encode_utf16().collect();
                                TextOutW(mem_dc, x + (34.0 * s) as i32, iy, &line_w);
                            } else {
                                let line_w: Vec<u16> = it.display_name.encode_utf16().collect();
                                TextOutW(mem_dc, x + (12.0 * s) as i32, py + ((rh - item_pt) / 2), &line_w);
                            }
                            SelectObject(mem_dc, l_old);
                        }
                        py += rh;
                    }
                    if max_scroll > 0 {
                        let sb_x = right - (10.0 * s) as i32;
                        let sb_top = y + ct;
                        let sb_h = (content_bottom - sb_top).max(20);
                        let thumb_h = ((visible_rows as f32 / total as f32) * sb_h as f32).max(16.0) as i32;
                        let thumb_y = sb_top + ((scroll as f32 / max_scroll as f32) * (sb_h - thumb_h) as f32) as i32;
                        let thumb_brush = CreateSolidBrush(rgb(130, 140, 165));
                        let thw = ((5.0 * s) as i32).max(3);
                        let th_rect = RECT { left: sb_x, top: thumb_y, right: sb_x + thw, bottom: thumb_y + thumb_h };
                        FillRect(mem_dc, &th_rect, thumb_brush);
                        DeleteObject(thumb_brush.into());
                    }
                }
            }

            DeleteObject(title_font.into());
            DeleteObject(grid_font.into());
            DeleteObject(item_font.into());
        }
    }

    /// 在 damage 区域内为卡片矩形 (x, y, w, h) 写入 per-pixel alpha：SDF 抗锯齿圆角 + 1px 亮色描边
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
        let bw = 0.9_f32; // 描边宽度（像素）
        unsafe {
        for &(rx, ry, rw, rh) in rects {
            let cx0 = rx.max(0);
            let cy0 = ry.max(0);
            let cx1 = (rx + rw).min(w);
            let cy1 = (ry + rh).min(h);
            if cx1 <= cx0 || cy1 <= cy0 {
                continue;
            }
            let r = (radius as f32).clamp(0.0, ((cx1 - cx0) as f32 / 2.0).min((cy1 - cy0) as f32 / 2.0));
            let fcx0 = cx0 as f32;
            let fcy0 = cy0 as f32;
            let fcx1 = cx1 as f32;
            let fcy1 = cy1 as f32;
            for &(dx0, dy0, dx1, dy1) in damage {
                let rx0 = cx0.max(dx0);
                let ry0 = cy0.max(dy0);
                let rx1 = cx1.min(dx1);
                let ry1 = cy1.min(dy1);
                if rx1 <= rx0 || ry1 <= ry0 {
                    continue;
                }
                for y in ry0..ry1 {
                    let Some((xs, xe)) = rounded_row(y, cx0, cy0, cx1, cy1, r as i32) else { continue };
                    let xs = xs.max(rx0);
                    let xe = xe.min(rx1);
                    if xe <= xs {
                        continue;
                    }
                    let row = (y * w) as usize;
                    let py = y as f32 + 0.5;
                    for x in xs..xe {
                        let sd = rounded_sd(x as f32 + 0.5, py, fcx0, fcy0, fcx1, fcy1, r);
                        let off = (row + x as usize) * 4;
                        if sd < -bw {
                            // 纯内部：保留绘制内容，只设透明度
                            *data.add(off + 3) = alpha;
                        } else {
                            // 描边环 + 抗锯齿过渡
                            let cov = (0.5 - sd).clamp(0.0, 1.0);
                            let a = (border_alpha as f32 * cov) as u8;
                            *data.add(off) = bdb;
                            *data.add(off + 1) = bdg;
                            *data.add(off + 2) = bdr;
                            *data.add(off + 3) = a;
                        }
                    }
                }
            }
        }
        }
    }
}

/// 无图标时的占位徽标：圆角方块 + 首字符（避免"很多图标空白"）
fn draw_fallback_badge(mem_dc: HDC, x: i32, y: i32, sz: i32, it: &DesktopItem, _seed: i32) {
    unsafe {
        let s = sz.max(12);
        let brush = CreateSolidBrush(rgb(86, 156, 214));
        let rect = RECT { left: x, top: y, right: x + s, bottom: y + s };
        let pen_old = SelectObject(mem_dc, GetStockObject(NULL_PEN));
        FillRect(mem_dc, &rect, brush);
        SelectObject(mem_dc, pen_old);
        DeleteObject(brush.into());
        // 首字符白字居中
        let ch: Vec<u16> = it.display_name.chars().next().map(|c| c.to_string()).unwrap_or_else(|| "?".into()).encode_utf16().collect();
        let f = make_font((s as f32 * 0.5) as i32, true);
        let old = SelectObject(mem_dc, f.into());
        SetTextColor(mem_dc, rgb(238, 240, 246));
        let mut tsz = SIZE::default();
        GetTextExtentPoint32W(mem_dc, &ch, &mut tsz);
        TextOutW(mem_dc, x + (s - tsz.cx) / 2, y + (s - tsz.cy) / 2, &ch);
        SelectObject(mem_dc, old);
        DeleteObject(f.into());
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