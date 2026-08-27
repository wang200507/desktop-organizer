use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use crate::layout::{Card, CardStyle};
use crate::scanner::{DesktopItem, IconKind};

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
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
    last_bbox: Option<(i32, i32, i32, i32)>,
    bg_color: u32,
}

impl Renderer {
    pub fn new() -> Self {
        unsafe {
            let title_font = CreateFontW(
                17, 0, 0, 0, FW_BOLD.0 as i32, 0, 0, 0,
                DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, DEFAULT_QUALITY,
                (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
                w!("Microsoft YaHei"),
            );
            let item_font = CreateFontW(
                15, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
                DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, DEFAULT_QUALITY,
                (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
                w!("Microsoft YaHei"),
            );
            let grid_font = CreateFontW(
                12, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
                DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, DEFAULT_QUALITY,
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
                last_bbox: None,
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

    pub fn render(&mut self, hwnd: HWND, cards: &[Card], items: &[DesktopItem], visible: bool, alpha: u8, show_icons: bool, selected: Option<(usize, usize)>) {
        unsafe {
            let w = GetSystemMetrics(SM_CXSCREEN);
            let h = GetSystemMetrics(SM_CYSCREEN);
            if w <= 0 || h <= 0 {
                return;
            }
            self.ensure_surface(w, h);
            let mem_dc = self.mem_dc.unwrap();

            // 清空 DIB
            std::ptr::write_bytes(self.bits as *mut u8, 0, (w * h * 4) as usize);
            SetBkMode(mem_dc, TRANSPARENT);

            if !visible {
                // 桌面已隐藏：窗口全屏，中央画一个"恢复提示"小卡片（双击恢复）
                SetWindowPos(
                    hwnd,
                    None,
                    0,
                    0,
                    w,
                    h,
                    SET_WINDOW_POS_FLAGS(0x0004 | 0x0010),
                );
                let hint_w = 280;
                let hint_h = 64;
                let cx = (w - hint_w) / 2;
                let cy = (h - hint_h) / 2;
                let card_brush = CreateSolidBrush(rgb(38, 42, 54));
                let rect = RECT { left: cx, top: cy, right: cx + hint_w, bottom: cy + hint_h };
                FillRect(mem_dc, &rect, card_brush);
                DeleteObject(card_brush.into());
                let old_font = SelectObject(mem_dc, self.title_font.into());
                SetTextColor(mem_dc, rgb(240, 240, 248));
                let hint: Vec<u16> = "桌面已隐藏 · 双击这里恢复".encode_utf16().collect();
                TextOutW(mem_dc, cx + 20, cy + 22, &hint);
                SelectObject(mem_dc, old_font);
                let hint_card = Card {
                    kind: IconKind::App,
                    title: String::new(),
                    x: cx,
                    y: cy,
                    width: hint_w,
                    height: hint_h,
                    item_indices: vec![],
                    style: CardStyle::List,
                    scroll: 0,
                };
                set_alpha(self.bits, w, h, &[hint_card], 200);
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let src_pt = POINT { x: 0, y: 0 };
                let size = SIZE { cx: w, cy: h };
                let _ = UpdateLayeredWindow(
                    hwnd,
                    None,
                    None,
                    Some(&size),
                    Some(mem_dc),
                    Some(&src_pt),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
                return;
            }

            // 正常渲染：计算包围盒，窗口跟随卡片
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
            SetWindowPos(
                hwnd,
                None,
                ox,
                oy,
                cw,
                ch,
                SET_WINDOW_POS_FLAGS(0x0004 | 0x0010),
            );

            let bg = self.bg_color;
            let card_brush = CreateSolidBrush(rgb((bg >> 16) as u8, (bg >> 8) as u8, bg as u8));

            for (ci, card) in cards.iter().enumerate() {
                // 卡片背景
                let old_brush = SelectObject(mem_dc, card_brush.into());
                let rect = RECT {
                    left: card.x,
                    top: card.y,
                    right: card.x + card.width,
                    bottom: card.y + card.height,
                };
                FillRect(mem_dc, &rect, card_brush);
                SelectObject(mem_dc, old_brush);

                // 标题居中（避开右侧按钮区）
                SetTextColor(mem_dc, rgb(240, 240, 248));
                let old_font = SelectObject(mem_dc, self.title_font.into());
                let title = format!("{} ({})", card.title, card.item_indices.len());
                let title_w: Vec<u16> = title.encode_utf16().collect();
                let mut sz = SIZE::default();
                GetTextExtentPoint32W(mem_dc, &title_w, &mut sz);
                let title_right = card.x + card.width - 64;
                let tx = card.x + (title_right - card.x - sz.cx) / 2;
                TextOutW(mem_dc, tx, card.y + 10, &title_w);
                SelectObject(mem_dc, old_font);

                // 右上角 X 按钮（大号）
                let x_old_font = SelectObject(mem_dc, self.title_font.into());
                SetTextColor(mem_dc, rgb(255, 130, 130));
                let x_w: Vec<u16> = "×".encode_utf16().collect();
                TextOutW(mem_dc, card.x + card.width - 26, card.y + 4, &x_w);
                SelectObject(mem_dc, x_old_font);

                // 样式切换按钮（X 左侧）：List 显示 ▦（点它切网格），Grid 显示 ≡（点它切列表）
                let style_old_font = SelectObject(mem_dc, self.title_font.into());
                SetTextColor(mem_dc, rgb(160, 180, 220));
                let style_icon: Vec<u16> = match card.style {
                    CardStyle::Grid => "≡".encode_utf16().collect(),
                    CardStyle::List => "▦".encode_utf16().collect(),
                };
                TextOutW(mem_dc, card.x + card.width - 54, card.y + 4, &style_icon);
                SelectObject(mem_dc, style_old_font);

                // 内容：Grid 网格 / List 列表
                match card.style {
                    CardStyle::Grid => {
                        // 图标网格：固定格宽 100，图标固定 32x32 自动排列补位，支持滚动
                        let cell_w = 100;
                        let cell_h = 72;
                        let cols = ((card.width - 16) / cell_w).max(1);
                        let start_x = card.x + 8;
                        let start_y = card.y + 40;
                        let content_bottom = card.y + card.height - 6;
                        let rows_visible = ((content_bottom - start_y) / cell_h).max(1);
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
                            let gx = start_x + col * cell_w;
                            let gy = start_y + row * cell_h;
                            if gy + cell_h > content_bottom {
                                break;
                            }
                            if let Some(it) = items.get(idx) {
                                if selected == Some((ci, ii)) {
                                    let sel_brush = CreateSolidBrush(rgb(70, 96, 140));
                                    let sel_rect = RECT { left: gx - 2, top: gy - 2, right: gx + cell_w - 2, bottom: gy + cell_h - 2 };
                                    FillRect(mem_dc, &sel_rect, sel_brush);
                                    DeleteObject(sel_brush.into());
                                }
                                if let Some(icon) = it.icon {
                                    let _ = DrawIconEx(mem_dc, gx + (cell_w - 32) / 2, gy + 6, icon, 32, 32, 0, None, DI_NORMAL);
                                }
                                let g_old_font = SelectObject(mem_dc, self.grid_font.into());
                                SetTextColor(mem_dc, rgb(220, 224, 232));
                                // 名字截断（超格宽显示 …）
                                let short: String = it.display_name.chars().take(6).collect();
                                let mut t: Vec<u16> = short.encode_utf16().collect();
                                if it.display_name.chars().count() > 6 {
                                    t.extend("…".encode_utf16());
                                }
                                TextOutW(mem_dc, gx + 4, gy + 42, &t);
                                SelectObject(mem_dc, g_old_font);
                            }
                        }
                        // 滚动条
                        if max_scroll > 0 {
                            let sb_x = card.x + card.width - 14;
                            let sb_top = card.y + 40;
                            let sb_h = (card.height - 46).max(20);
                            let thumb_h = ((rows_visible as f32 / total_rows as f32) * sb_h as f32).max(16.0) as i32;
                            let thumb_y = sb_top + ((scroll as f32 / max_scroll as f32) * (sb_h - thumb_h) as f32) as i32;
                            let sb_brush = CreateSolidBrush(rgb(60, 66, 80));
                            let sb_rect = RECT { left: sb_x, top: sb_top, right: sb_x + 6, bottom: sb_top + sb_h };
                            FillRect(mem_dc, &sb_rect, sb_brush);
                            let thumb_brush = CreateSolidBrush(rgb(120, 130, 150));
                            let th_rect = RECT { left: sb_x, top: thumb_y, right: sb_x + 6, bottom: thumb_y + thumb_h };
                            FillRect(mem_dc, &th_rect, thumb_brush);
                            DeleteObject(sb_brush.into());
                            DeleteObject(thumb_brush.into());
                        }
                    }
                    CardStyle::List => {
                        // 标题列表（字体放大 15px），支持 scroll 滚动
                        let row_h = if show_icons { 24 } else { 20 };
                        SelectObject(mem_dc, self.item_font.into());
                        SetTextColor(mem_dc, rgb(220, 224, 232));
                        let content_bottom = card.y + card.height - 6;
                        let visible_rows = ((content_bottom - (card.y + 40)) / row_h).max(1);
                        let total = card.item_indices.len() as i32;
                        let max_scroll = (total - visible_rows).max(0);
                        let scroll = card.scroll.clamp(0, max_scroll);
                        let mut y = card.y + 40;
                        for (ii, &idx) in card.item_indices.iter().enumerate() {
                            if (ii as i32) < scroll {
                                continue;
                            }
                            if y > content_bottom {
                                break;
                            }
                            if let Some(it) = items.get(idx) {
                                if selected == Some((ci, ii)) {
                                    let sel_brush = CreateSolidBrush(rgb(70, 96, 140));
                                    let sel_rect = RECT { left: card.x + 6, top: y - 1, right: card.x + card.width - 32, bottom: y + row_h - 1 };
                                    FillRect(mem_dc, &sel_rect, sel_brush);
                                    DeleteObject(sel_brush.into());
                                }
                                if show_icons {
                                    if let Some(icon) = it.icon {
                                        let _ = DrawIconEx(mem_dc, card.x + 12, y, icon, 16, 16, 0, None, DI_NORMAL);
                                    }
                                    let line_w: Vec<u16> = it.display_name.encode_utf16().collect();
                                    TextOutW(mem_dc, card.x + 34, y + 2, &line_w);
                                } else {
                                    let line_w: Vec<u16> = it.display_name.encode_utf16().collect();
                                    TextOutW(mem_dc, card.x + 12, y + 2, &line_w);
                                }
                            }
                            y += row_h;
                        }
                        // 滚动条：内容超出时右侧画指示条
                        if max_scroll > 0 {
                            let sb_x = card.x + card.width - 14;
                            let sb_top = card.y + 40;
                            let sb_h = (card.height - 46).max(20);
                            let thumb_h = ((visible_rows as f32 / total as f32) * sb_h as f32).max(16.0) as i32;
                            let thumb_y = sb_top + ((scroll as f32 / max_scroll as f32) * (sb_h - thumb_h) as f32) as i32;
                            let sb_brush = CreateSolidBrush(rgb(60, 66, 80));
                            let sb_rect = RECT { left: sb_x, top: sb_top, right: sb_x + 6, bottom: sb_top + sb_h };
                            FillRect(mem_dc, &sb_rect, sb_brush);
                            let thumb_brush = CreateSolidBrush(rgb(120, 130, 150));
                            let th_rect = RECT { left: sb_x, top: thumb_y, right: sb_x + 6, bottom: thumb_y + thumb_h };
                            FillRect(mem_dc, &th_rect, thumb_brush);
                            DeleteObject(sb_brush.into());
                            DeleteObject(thumb_brush.into());
                        }
                    }
                }
            }

            DeleteObject(card_brush.into());
            set_alpha(self.bits, w, h, cards, alpha);

            // UpdateLayeredWindow 提交窗口区域（窗口已移动到包围盒）
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

/// 合并两个包围盒（取并集）
fn union_bbox(
    a: Option<(i32, i32, i32, i32)>,
    b: Option<(i32, i32, i32, i32)>,
) -> Option<(i32, i32, i32, i32)> {
    match (a, b) {
        (Some((ax0, ay0, ax1, ay1)), Some((bx0, by0, bx1, by1))) => {
            Some((ax0.min(bx0), ay0.min(by0), ax1.max(bx1), ay1.max(by1)))
        }
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn set_alpha(bits: *mut std::ffi::c_void, w: i32, h: i32, cards: &[Card], alpha: u8) {
    let data = bits as *mut u8;
    for card in cards {
        // clamp 到屏幕内，避免遍历屏幕外区域（往外拖时卡顿）
        let x0 = card.x.max(0);
        let y0 = card.y.max(0);
        let x1 = (card.x + card.width).min(w);
        let y1 = (card.y + card.height).min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                let off = ((y * w + x) * 4) as usize;
                unsafe { *data.add(off + 3) = alpha; }
            }
        }
    }
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
