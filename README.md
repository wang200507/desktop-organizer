<p align="center">
  <img src="https://img.icons8.com/color/96/desktop.png" width="64" />
  <h1 align="center">Desktop Organizer</h1>
  <p align="center">Rust 原生 · 低内存桌面图标自动整理工具</p>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-stable-orange" />
  <img src="https://img.shields.io/badge/Windows-10%2F11-blue" />
  <img src="https://img.shields.io/badge/Memory-~9MB-1f9d55" />
  <img src="https://img.shields.io/badge/Size-~310KB-brightgreen" />
  <img src="https://img.shields.io/badge/LICENSE-MIT-db4b2b" />
</p>

🔖 自动扫描分类 · ✅ 扁平化圆角卡片 · 🖱️ 全局鼠标钩子 · 🔄 双击隐藏/恢复 · 🐶 watchdog 守护 · 🎯 damage 局部重绘（拖拽 3.4× 流畅） · 🎨 网格/列表双模式 · 🚀 开机自启 + 一键卸载

---

## 🧩 这是什么

一个类 **cooDesker / Fences** 的 Windows 桌面图标自动整理工具。用 **Rust** 编写，基于 **GDI + 分层窗口** 渲染半透明圆角分区卡片，把桌面图标按类型自动归档到可自由拖拽的卡片中。

> 💡 **一句话**：扫描桌面 → 按类型分进圆角扁平卡片 → 隐藏系统图标层接管桌面 → 一切后台静默运行，常驻内存约 **9MB**。

> 📦 **Rust 原生无 GC**：无 .NET / Electron / JVM 运行时包袱，静态链接 CRT，exe 仅约 310KB，后台运行几乎无感。
>
> 🎨 **GDI 软件渲染**：不用 Direct2D/Direct3D，不加载 GPU 驱动栈（省 30+MB），同样实现数学圆角裁形 + per-pixel alpha 半透明。
>
> 🐶 **watchdog 守护**：主进程被强杀 → 守护子进程自动恢复桌面图标，不会留下"图标消失"的尴尬。
>
> 🔄 **damage 局部重绘**：拖拽/滚轮/选中只清空重绘受影响区域，不再整屏 memset + 全量重绘，实测拖拽 300 步 CPU **~78ms**（旧版 ~266ms，3.4× 优化）。

---

## 🚀 快速开始

### 方式一：下载 Release（推荐）

最新版可执行文件（Windows 10/11 x64，免安装）：

[⬇️ 下载 desktop-organizer.exe](https://github.com/wang200507/desktop-organizer/releases/latest/download/desktop-organizer.exe)

> 首次运行 Windows SmartScreen 可能提示拦截（未数字签名），点「更多信息 → 仍要运行」即可。

### 方式二：源码构建

环境要求：Rust stable MSVC 工具链 + VS Build Tools（C++ 工作负载 + Windows SDK）。

```bash
cargo build --release
```

产物：`target/release/desktop-organizer.exe`

---

## 🎮 操作指南

```bash
desktop-organizer.exe
```

| 操作 | 效果 |
|------|------|
| 拖拽卡片标题栏 | 移动卡片 |
| 拖拽卡片右/下边缘 | 调整卡片大小 |
| 双击桌面空白 | 隐藏/显示所有卡片 |
| 点卡片右上角 ✕ | 删除该分区 |
| 点标题栏 ▦ / ≡ | 切换 网格图标 / 列表 模式 |
| 鼠标滚轮 | 滚动卡片内容 |
| F5 | 一键归位（重排默认布局） |
| F1 | 设置界面 |
| Esc | 退出（自动恢复系统桌面图标） |
| 卡片内右键 | 删除分区 / 新建分区 / 设置 / 退出 |
| 桌面空白右键 | 系统原生菜单（查看/排序/刷新/新建/显示设置…） |

---

## ✨ 功能特性

- **自动扫描分类**：扫描桌面快捷方式/文件，按 应用 / 文件夹 / 文档 / 图片 / 其他 自动分组
- **扁平化圆角卡片**：数学圆角裁形 + 1px 自动提亮描边 + 强调圆点 + 分隔线，per-pixel alpha 半透明
- **卡片自由拖拽**：按住标题栏移动；右/下边缘调整大小；拖动自动置顶
- **damage 局部重绘**：拖拽/滚轮/选中只重绘受影响区域，流畅不卡顿
- **双模式切换**：网格图标模式 / 列表模式（可滚动、扁平滚动条）
- **位置记忆**：位置与尺寸持久化到 `layout.json`，重启自动恢复
- **双击隐藏**：全局钩子检测桌面双击隐藏/显示，隐藏态双击提示卡即可恢复
- **原生桌面右键**：桌面空白右键弹系统原生菜单（Shell COM），卡片内右键弹自定义菜单
- **桌面接管**：隐藏系统图标层，卡片显示在壁纸之上、正常窗口之下
- **watchdog 守护**：程序被强杀后自动恢复桌面图标
- **开机自启 + 一键卸载**：F1 设置界面开关，注册表 Run 键，卸载清理数据目录
- **32px 大图标**：解析 .lnk 取真实图标（去快捷方式箭头），网格模式清晰

---

## 🛠️ 技术方案（低内存核心）

```
Rust 原生码（无 GC/VM，静态链接 CRT）
  └─ GDI 纯软件渲染
       └─ WS_EX_LAYERED 分层窗口 + UpdateLayeredWindow(ULW_ALPHA | AC_SRC_ALPHA)
            └─ 32 位 DIB per-pixel alpha 半透明圆角卡片（数学圆角裁形 + 1px 亮色描边）
                 └─ 窗口跟随卡片包围盒，只提交包围盒区域合成
                      └─ 拖拽/滚轮/选中走 damage 局部重绘（清空-裁剪-重绘-混成只碰受影响区域）
```

**为什么不用 Direct2D**：实测 Direct2D 需加载 d3d11/dxgi/d2d1/dwrite 整套 GPU 驱动栈，私有内存 **46MB**（超标 3 倍）；而 GDI + 分层窗口仅 **9MB**，同样能实现半透明圆角。这是 Fences / cooDesker 类工具的实际做法。

**为什么不用 Electron/WPF/Java**：解释器/运行时起步 50~200MB，直接出局。

---

## 📊 实测性能

| 指标 | 实测值 |
|------|--------|
| 私有内存（常驻） | **~9 MB** |
| exe 体积 | **~310 KB**（release: opt-level z + lto + strip） |
| 拖拽 300 步 CPU | **~78 ms**（旧版 ~266 ms，**3.4×** 优化） |
| 渲染方式 | GDI 软件渲染 + damage 局部重绘 |

---

## 📁 模块结构

```
src/
├── scanner.rs   桌面扫描与分类（图标获取、.lnk 解析去箭头）
├── layout.rs    分区模型、布局、命中测试、JSON 持久化
├── renderer.rs  分层窗口渲染（GDI + UpdateLayeredWindow + damage 局部重绘）
├── settings.rs  配置结构（透明度/背景色/显示格式等）
└── main.rs      窗口、事件交互、全局鼠标钩子、watchdog
```

---

## 🔧 数据目录

`%LOCALAPPDATA%\DesktopOrganizer\`（删除即恢复默认）

| 文件 | 说明 |
|------|------|
| layout.json | 卡片位置/尺寸/滚动偏移/样式 |
| settings.json | 透明度/背景色/自动分类/开机自启 |
| error.log | 仅错误时写入（无调试日志） |

---

## 🤝 贡献

欢迎 Fork / Issue / PR！

- Windows 10 兼容性验证
- 多显示器布局优化
- 图标拖拽跨分区
- 更多分类规则

## 📄 许可证

本项目基于 **MIT License** 开源，详见 [LICENSE](./LICENSE) 文件。
