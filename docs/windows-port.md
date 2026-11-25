# herdr-pet Windows 适配方案

> 状态：M0–M2 已通过 Windows 实机验收（多显示器项按用户要求跳过；M2 提示音待听感
> 确认）；M3 已实现并完成合成鼠标验证，第 7–8 项的真实鼠标验收待用户确认。M4 未
> 开始。实施期间本文作为 Windows 平台的设计、里程碑和验收依据。
>
> M0/M1 实施记录（2026-11-25）：Windows CI（`windows-latest`）通过 fmt、clippy
> `-D warnings`、测试与 release 构建；产物在 Windows 11（3600×2260，200% 缩放）
> 实机运行。M0 交付：透明窗口与 DirectComposition 渲染、全局指针拖拽、基于窗口
> region 的逐像素点击穿透、彩色托盘图标、`hc connect` 传输与重连、Windows CI
> 构建 job。M1 交付：配置迁移到 `%APPDATA%`（一次性移动旧 `~/.config` 文件）、
> 按工作区计算的默认位置、跨 DPI 拖动、托盘/宠物菜单中的点击穿透开关。
>
> 实机验收（2026-11-25，真实鼠标）：
>
> - 透明像素点击穿透，宠物本体可拖动（验收清单第 5 项）；
> - 托盘/宠物菜单的「点击穿透」开关生效：开启后点击穿透且宠物保持可见，关闭后
>   恢复拖动；
> - 任务栏无图标、Alt+Tab 无条目，重启后宠物位置保持（验收清单第 2、4 项）。
>
> M2 实机验证（2026-11-25）：点击宠物拉起 Herdr 并聚焦当前 agent；blocked 状态
> 触发 Toast（显示“osc-test2 等待输入”，`LastNotificationAddedTime` 同步更新）；
> 提示音走系统别名，待用户确认听感。
>
> 多显示器/混合 DPI（第 3 项）按用户要求跳过，未验证。此前合成点击的辅助验证：
> 窗口 region 生效（COMPLEXREGION，约 440 个矩形），透明区域的点击送达另一进程
> 窗口，宠物本体不穿透；仅 `WS_EX_TRANSPARENT` 的整窗开关无效，改为
> `WS_EX_TRANSPARENT | WS_EX_LAYERED` +
> `SetLayeredWindowAttributes(255, LWA_ALPHA)` 后，命中测试忽略整窗，且
> DirectComposition 内容截图前后逐像素一致。
>
> M3 实机验证（2026-11-25，合成鼠标）：Direct2D 气泡正常显示（粗体标题、副标题、
> Agent 图标、done/blocked 状态图标、working 旋转 spinner、分页箭头、展开/收起
> 按钮；长标题带省略号）；靠近屏幕顶边时气泡翻到宠物下方并重排按钮；点击气泡拉起
> Herdr 并聚焦当前 agent；点击穿透像素按预期落到下层窗口。设置面板从宠物右键菜单
> 打开，六个开关、大小与不透明度滑杆、目录选择、面板外点击关闭均正常，修改即时
> 生效并写入配置；“关于”显示版本与仓库地址。第 7–8 项的**真实鼠标**验收待用户
> 确认。
>
> 已知限制：无真实背景模糊（半透明填充）；设置面板为经典 Win32 浅色外观（不随
> 系统深色模式换肤）；“关于”暂用 MessageBox，TaskDialog 需 M4 的 comctl32 v6
> manifest；Toast 暂用系统默认图标（exe 图标随 M4）；多显示器混合 DPI 未验证
> （按用户要求跳过）。

## 1. 已确认决策

| 决策点 | 选择 |
| --- | --- |
| 发布形态 | 单个便携 exe，不使用安装器，不额外提供 zip |
| 运行验证 | 有 Windows 实机；每个里程碑产出 exe 后由用户手动验收 |
| 气泡视觉 | 首版接受半透明填充，不实现真实背景模糊 |
| 平台基线 | Windows 10 1809+ / Windows 11，x64；ARM64、Linux 不在首版范围 |
| 验证方式 | 通过 Windows 本机构建或 CI 产出 exe，用户在实机执行本文第 7 节的验收清单 |

目标是功能对齐当前 macOS 版本；平台差异（无背景模糊、无虚拟桌面置顶、通知能力受限）在 README 中明确标注，不静默缺失。

## 2. 关键发现与硬约束

### 2.1 现有平台隔离

- `main.rs`、`status.rs`、`pet.rs`、`chat.rs`、`config.rs` 基本与平台无关。
- `src/platform/mod.rs:29` 已为 `not(macos)` 提供全部 stub；新增 `src/platform/windows.rs` 后，主逻辑不需要分叉。
- 真正需要改写的平台代码集中在 `src/platform/macos.rs`（1417 行 AppKit）。
- 唯一 Unix 硬依赖是 `src/herdr.rs:14` 的 `tokio::net::UnixStream`，以及 `READ_TIMEOUT` / socket 发现逻辑。

### 2.2 wgpu 透明窗口

wgpu 30 的 DX12 后端对普通 HWND surface 只声明 `CompositeAlphaMode::Opaque`
（`wgpu-hal-30.0.2/src/dx12/adapter.rs:1365`）。因此：

- 普通 swapchain + `with_transparent(true)` 在 Windows 上会得到不透明黑底；
- 必须使用 DirectComposition visual：`Dx12BackendOptions.presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual`；
- 该路径支持 `PreMultiplied` alpha，但不被 RenderDoc 支持（仅开发工具限制）。

示意：

```rust
let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
#[cfg(windows)]
{
    descriptor.backend_options.dx12.presentation_system =
        wgpu::Dx12SwapchainKind::DxgiFromVisual;
}
let instance = wgpu::Instance::new(descriptor);
```

这是整个 Windows 适配的 go/no-go 点，必须在 M0 用真实 Windows 机器验证。

### 2.3 herdr-core 传输

`hc connect` 的 stdio JSON-RPC 已在本机实测可用：

- 发送 `hello` 与 `events.subscribe` 后，能从 stdout 收到响应与终端快照；
- Windows 上 herdr-core 的安装位置与 IPC 形式未在本机验证，首版不依赖 named pipe / `HC_TERMINAL_SOCKET`；
- 通过 `hc connect --session <session>` 子进程连接，断线后杀进程重连即可。

静态检查基线（2026-11-25）：在 macOS 上安装 `x86_64-pc-windows-msvc` 后执行
`cargo check --target x86_64-pc-windows-msvc`，当前仅 `src/herdr.rs` 的
`tokio::net::UnixStream` / `unix::OwnedWriteHalf` 共 4 处报错；其余主程序代码与
wgpu、winit、tray-icon、muda 等依赖在 Windows 目标下均可编译通过。说明移植前的
Windows 编译阻塞点只有 herdr 传输层，没有额外的依赖或条件编译问题。

### 2.4 Windows API 支持

- `winit 0.30`：`with_skip_taskbar`、`with_no_redirection_bitmap`、`set_cursor_hittest` 均可用；后者在 Windows 上添加 `WS_EX_TRANSPARENT | WS_EX_LAYERED`（`winit window_state.rs:300`）。
- `tray-icon 0.26`：Windows `set_visible` 已实现；`muda 0.21`：支持 `show_context_menu_for_hwnd`。
- `winit` 在 Windows 上设置 Per-Monitor V2 DPI awareness，`scale_factor()`、物理坐标与 `set_outer_position` 可直接使用。

## 3. 总体架构

```text
herdr-core（Windows）
        │  hc connect --session <session>（stdio JSON-RPC）
        ▼
  herdr.rs（后台 tokio 任务，跨平台传输抽象）
        │  Snapshot / Event / Disconnected
        ▼
  status.rs + pet.rs + chat.rs（现有纯逻辑，不改）
        ▼
  main.rs（winit 主线程，60 Hz tick）
        ├── render.rs：DxgiFromVisual 透明 surface + 宠物 quad + 气泡纹理 quad
        ├── platform/windows.rs
        │     ├── 鼠标/拖拽/点击穿透/工作区
        │     ├── Direct2D + DirectWrite 气泡位图
        │     ├── Win32 设置面板
        │     ├── 托盘/通知/关于/右键菜单
        │     └── Herdr 前台判断与路径发现
        ├── notify.rs：Toast + PlaySoundW
        └── tray.rs：彩色托盘图标（Windows）
```

Windows 上不引入第二套 GUI 框架：气泡用 Direct2D/DirectWrite 栅格化后交给 wgpu 合成，设置面板用原生 Win32 控件。

## 4. 模块方案

### 4.1 平台层拆分

新增 `src/platform/windows.rs`，在 `src/platform/mod.rs` 中按 `cfg(target_os = "windows")` 导出，与 macOS 保持相同接口。

`platform/mod.rs` 现有的 `#[cfg(not(target_os = "macos"))]` stub 必须改成 `#[cfg(not(any(target_os = "macos", target_os = "windows")))]`，否则 Windows 会同时编译 stub 和真实实现；macOS/Windows 各自只编译自己的模块。

| macOS | Windows |
| --- | --- |
| `mouse_position` | `GetCursorPos`（物理像素、左上角原点，无需主屏换算） |
| `left_button_down` | `GetAsyncKeyState(VK_LBUTTON)` |
| `menu_bar_height` / 工作区 | 新增 `work_area`：`GetMonitorInfoW().rcWork`；任务栏在任意边都不进入 |
| `herdr_app_frontmost` | `GetForegroundWindow` + 进程映像路径/名称比较 |
| `herdr_app_path` | PATH、`%LOCALAPPDATA%\Programs\...`、App Paths 注册表 |
| `send_notification` | WinRT Toast（AUMID） |
| `show_about` | TaskDialog（回落 MessageBox） |
| `show_context_menu` | `muda::ContextMenu::show_context_menu_for_hwnd` |
| `float_on_all_spaces` | no-op（Windows 无等价 Spaces） |
| `allow_window_above_menu_bar` | no-op |
| `ChatOverlay` | Direct2D 位图 + wgpu 纹理 |
| `SettingsPanel` | Win32 无模式窗口 + 标准控件 |

为减少重复：

- 把设置面板的 `UiState` / `diff` 从 `platform/macos.rs` 提取到 `platform/mod.rs`，两端共用；
- 在 `platform/mod.rs` 增加 `work_area(monitor) -> Option<Rect>`：Windows 返回 `rcWork`，macOS 返回 `None`，`main.rs` 的 `screen_area` 保持 macOS 原行为；
- 增加 `platform::set_cursor_hittest(window, bool)` 薄封装，Windows 可在此替换实现，macOS 继续调用 winit。

### 4.2 窗口、渲染与输入

`main.rs` 的窗口属性：

- Windows 加 `with_skip_taskbar(true)`；
- 候选 `with_no_redirection_bitmap(true)`，配合 DirectComposition；该选项与点击穿透的兼容性在 M0 验证；
- 创建后追加 `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`；`with_skip_taskbar` 只通过 ITaskbarList 移除任务栏按钮，Alt+Tab 隐藏仍需 `WS_EX_TOOLWINDOW`；点击宠物不抢焦点，避免干扰 Herdr 前台判断；
- `WindowLevel::AlwaysOnTop` 已映射 `HWND_TOPMOST`，暂不额外重设。

`render.rs`：

- Windows 实例启用 `DxgiFromVisual`；
- alpha mode：macOS 保持 `PostMultiplied`，Windows 使用 `PreMultiplied`；
- 保留现有 atlas 预乘 sRGB 方案，Windows 上需在浅色/深色壁纸与 100%/150%/200% 缩放下检查边缘；
- 新增 overlay 纹理与第二个 pipeline：预乘 alpha 混合（`ONE, ONE_MINUS_SRC_ALPHA`），在宠物 quad 之后绘制气泡位图；
- 纹理上传处理 `COPY_BYTES_PER_ROW_ALIGNMENT` 行对齐。

点击穿透（M0 实现）：

- 不复用 winit 的 `set_cursor_hittest`：它在 Windows 上给窗口加 `WS_EX_LAYERED`，与 DirectComposition 视觉冲突；仅切换 `WS_EX_TRANSPARENT` 也不够，ReactOS/Wine/GLFW/Electron 的跨进程命中穿透都要求 `WS_EX_LAYERED | WS_EX_TRANSPARENT` 同时存在；
- 改用操作系统窗口 region：按当前宠物帧的 atlas alpha（阈值 16）生成水平 run，经 `ExtCreateRegion` + `SetWindowRgn` 设置窗口区域；区域外的透明像素在 win32k 命中测试中被排除，实现跨进程点击穿透。这与 Chromium 对其 Direct3D 透明窗口使用的方案一致；
- region 只在帧、宠物矩形或缩放变化时重建（`main.rs` 的 `hit_region_key`）。窗口区域同时裁剪绘制，alpha < 16 的不可见边缘会被裁掉；阈值与点击判定一致，M0 实机确认视觉无差异；
- `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` 通过窗口子类的 `WM_STYLECHANGING` 保持，避免被 winit 的整体样式重写抹掉；
- 整窗“点击穿透”开关使用 `WS_EX_TRANSPARENT | WS_EX_LAYERED`，并调用
  `SetLayeredWindowAttributes(..., 255, LWA_ALPHA)`：实机验证表明仅
  `WS_EX_TRANSPARENT` 对非 layered 窗口不生效，点击仍被窗口接收；full-alpha 的
  layered surface 不改变 DirectComposition 内容（截图逐像素一致）；
- 窗口子类在 `WM_STYLECHANGING` 保持上述两个位，并在 `WM_STYLECHANGED` 重新应用
  全 alpha，避免 winit 重写样式后点击穿透失效或窗口不再显示；
- 宠物透明像素判定继续使用 atlas alpha（阈值 16）：macOS 使用 `alpha_at_view`，Windows 使用 `alpha_spans`。

屏幕坐标：

- 拖拽与凝视继续使用全局指针 + `set_outer_position`；
- 屏幕可用区域改用 `rcWork`，宠物不进入任务栏（任务栏在上/下/左/右均适用）；
- 多显示器负坐标、混合 DPI 由 `GetCursorPos` + winit 物理坐标覆盖；
- `config.position` 继续保存宠物左上角物理像素。

### 4.3 herdr-core 传输

把 `src/herdr.rs` 的传输抽象为枚举：

```rust
enum Transport {
    Unix(PathBuf),                  // 现有 UnixStream 路径
    HcConnect { program: PathBuf, session: String }, // Windows
}
```

- Unix：保留 `HC_TERMINAL_SOCKET`、`$XDG_STATE_HOME/herdr-core/<session>/<session>.sock` 发现顺序；
- Windows：`tokio::process::Command` 启动 `hc connect --session <session>`，stdout/stdin 作为读写半端；
- 子进程使用 `CREATE_NO_WINDOW` 和 `kill_on_drop(true)`；`missed`/超时/断线后杀掉子进程，沿用 2 秒重连；
- `read_response` / `read_line` 泛化为 `AsyncRead + AsyncWrite`；
- Windows 程序发现顺序：
  1. PATH 中的 `hc.exe` / `herdr-core.exe`；
  2. `%LOCALAPPDATA%\Programs\herdr-core\hc.exe`；
  3. `%LOCALAPPDATA%\Programs\herdr-core\herdr-core.exe`；
  4. 找不到时记录明确错误并重试，不退出主进程。
- 平台专用变体与函数按 `cfg` 收口，避免 `clippy -D warnings` 因死代码失败；
- `HC_SESSION` 继续作为 session 来源，默认 `default`。

测试：

- 现有 Unix socket 测试用 `#[cfg(unix)]` 收口，避免 Windows 编译失败；
- 新增基于 `tokio::io::duplex` 的跨平台握手测试；
- Windows 运行时由用户在实机验证（快照、事件、断线重连）。

### 4.4 Herdr.app 集成

`src/herdr_app.rs`：

- `app_home()`：
  - macOS 保持 `HERDR_APP_HOME` → `XDG_CONFIG_HOME/herdr-app` → `~/.config/herdr-app`；
  - Windows 与 macOS 相同（实机确认 `view.json`、`icons` 都在 `~/.config/herdr-app`，
    无需改用 `%APPDATA%`）。
- `focus_terminal`：
  - 不再使用 `/usr/bin/open -b dev.herdr.app`；
  - 启动找到的 `herdr-app.exe`：无参启动会恢复并聚焦已运行窗口（最小化后实测）；
  - 实机确认 CLI 为 `herdr-app.exe do agent.focus --terminal <address>`，
    `terminal.peek` 参数相同；
  - 子 agent 仍先 `terminal.peek`，失败回退 `agent.focus`；
  - 子进程用 `CREATE_NO_WINDOW`，保留 25 × 200ms 重试；
  - 发现顺序：PATH 的 `herdr-app.exe` / `Herdr.exe` →
    `%LOCALAPPDATA%\Programs\herdr-app\herdr-app.exe` →
    `%LOCALAPPDATA%\Programs\Herdr\Herdr.exe`（进程内缓存一次）。
- 前台判断：
  - `GetForegroundWindow` → `GetWindowThreadProcessId` → `QueryFullProcessImageNameW`；
  - 与 Herdr 可执行文件路径比较，路径不可得时比较文件名 `Herdr.exe` / `herdr-app.exe`；
  - 仍每 250ms 查询一次。
- `view.json` 已读逻辑不变（实机确认 Windows 的 `attention.seen` 结构一致）；
- 找不到 Herdr 时按现有策略降级：图标隐藏、点击只尝试拉起，不影响宠物和气泡。

### 4.5 聊天浮层

Windows 首版不追求真实模糊，使用半透明填充：

- 用 **Direct2D + DirectWrite** 把气泡、标题/副标题、状态图标、分页箭头、右下角按钮画进一张预乘 BGRA 位图；
- 位图裁剪到实际可见区域（气泡 + 按钮），通过 `ChatOverlay::take_bitmap()` 交给 `Renderer` 上传为 wgpu 纹理；
- macOS 的 `NSVisualEffectView` 保持不变，`take_bitmap()` 在 macOS 返回 `None`；
- 点击命中继续由 `main.rs` 的矩形判定处理，不需要第二套输入窗口。

实现细节（M3 已落地）：

- `ChatOverlay` 用 `ID2D1DCRenderTarget` 绑定 32 位 top-down DIB（预乘 BGRA），
  按窗口物理尺寸渲染后由 `take_bitmap()` 交给 wgpu 上传为 `Bgra8Unorm` 纹理，
  以 `ONE, ONE_MINUS_SRC_ALPHA` 混合画在宠物 quad 之后；
- 字体：DirectWrite `Microsoft YaHei UI`，粗体标题 + 常规正文；灰度抗锯齿避免透明
  背景上的 ClearType 彩边；正文用 `SetTrimming` 做尾部省略号；
- 主题：读取 `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme`，
  每 2 秒轮询一次；深色主题用浅色文字 + 深色玻璃，浅色主题反之；
- `chat_opacity` 只作用于玻璃层与描边，文字和图标保持不透明；
- Agent 图标：`resvg 0.50`（关闭默认特性）把 Herdr 的 SVG 栅格化为 2× 尺寸位图，
  `tint` 时按主题文字色在预乘空间改 RGB 并保留 alpha；栅格像素按路径/主题缓存，
  `ID2D1Bitmap` 每次绘制按当前渲染目标重新上传（D2D 资源不能跨目标复用），失败时
  隐藏图标并恢复标题宽度；
- 状态图标：用 Direct2D 基本图元绘制（Idle 用 “Z” 字形、Blocked 用感叹号、Done
  对勾、Error 叉、无 agent 省略号），不依赖图标字体码点；
- 工作中的 spinner：10 个半径渐变的圆点绕圈旋转，在 `take_bitmap()` 内以 33ms
  节流重绘，不额外开定时器；
- 气泡翻到下方、横移、收起、pager 出现/消失、窗口 DPI 改变时重新栅格化；
- 命中区域在宠物 alpha run 之外并入气泡与按钮矩形，绘制出来的 UI 才能收到点击；
- 不支持真实背景模糊，README 标注为 Windows 首版差异。

### 4.6 设置面板

用原生 Win32 无模式窗口，不引入第二套 GUI 框架（M3 已落地）：

- 自建窗口类 `herdr_pet_settings`，`WS_CAPTION | WS_SYSMENU` +
  `WS_EX_TOOLWINDOW`；打开时置顶并尝试 `SetForegroundWindow`，关闭按钮与 Esc
  只隐藏窗口，实例保留；
- 控件：`COMBOBOX`（宠物）、静态文本（目录，`SS_ENDELLIPSIS`）、两个按钮、
  `TRACKBAR`（大小 ×10、不透明度 ×100 的整数映射）、6 个 `BS_AUTOCHECKBOX`、
  底部说明；字体 `Microsoft YaHei UI` 9pt；
- 沿用 macOS 的轮询设计：每 tick 读取控件状态，与共享的 `UiState` diff 后生成
  `PanelAction`；程序化同步后更新 `last`，避免回环；
- 初始 DPI 取 `GetDpiForSystem()`；`WM_DPICHANGED` 应用系统建议矩形并记录新 DPI，
  下一次轮询重建字体、按逻辑坐标重排所有子控件；
- 目录选择：`IFileOpenDialog` + `FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM`。主线程
  已被 WinRT Toast 初始化为 MTA，因此选择器放在独立的 STA 线程，对话框以面板为
  owner；结果经 channel 回传，下一次轮询生效；
- 点击面板外关闭：300ms 延迟后轮询左键按下沿 + `GetWindowRect`；目录选择器打开
  期间用原子标志豁免；
- `click_through` 开启时禁用「隐藏托盘图标」勾选框，与 macOS 一致；
- “关于”用 `MessageBoxW` 显示版本与仓库地址（带 `MB_TOPMOST`）。TaskDialog 依赖
  comctl32 v6，留到 M4 的 manifest 一起启用。

### 4.7 托盘、通知与关于

托盘：

- `tray-icon` / `muda` 直接可用；
- macOS 继续使用模板图 `assets/tray-icon.png`；
- Windows 不使用黑色模板图（深色任务栏不可见），运行时把 `assets/app-icon.png` 缩放为 32×32 彩色图标；
- 宠物右键菜单使用 `show_context_menu_for_hwnd`，位置由 muda 取指针位置；
- `set_visible`、菜单事件轮询沿用现有逻辑。

通知与声音：

- Toast 用 `windows` crate 的 WinRT API，AUMID 为 `dev.herdr.pet`；
- 实机结论：单 exe 未注册 AUMID 时 `Show` 返回成功但系统静默丢弃；在
  `HKCU\Software\Classes\AppUserModelId\dev.herdr.pet` 写入 `DisplayName=herdr-pet`
  后 Toast 正常显示，无需 Start Menu 快捷方式，也无需设置进程 AUMID；
- 应用启动时注册一次；Toast XML 带 `<audio silent="true"/>`，声音只由配置控制；
- 首版使用系统默认图标，exe 图标随 M4 加入；
- 声音：`PlaySoundW` 系统别名，blocked → `SystemHand`，done → `SystemAsterisk`；
- `notify.rs` 按平台拆分：macOS 保留 osascript/afplay，Windows 走平台实现；
- `config.notifications` / `notification_sound` 语义保持不变。

关于：

- M3 使用 `MessageBoxW` 显示版本号（`CARGO_PKG_VERSION`）与仓库地址；M4 加入
  common-controls v6 manifest 后再升级为 `TaskDialog` + 可点击链接；
- 模态期间阻塞事件循环，与 macOS 现行为一致。

### 4.8 路径与配置

- `config.rs`：
  - macOS 保持 `$XDG_CONFIG_HOME/herdr-pet/config.json`（默认 `~/.config`）；
  - Windows 使用 `%APPDATA%\herdr-pet\config.json`；首次读取时若新位置不存在而旧位置
    （`$XDG_CONFIG_HOME` 或 `~/.config`）存在，则把旧文件移动到新位置（跨卷时复制后
    删除），避免丢失已有设置。
- 宠物目录：`CODEX_HOME` → `dirs::home_dir()/.codex/pets`；Windows 下即 `%USERPROFILE%\.codex\pets`，需在实机确认 Codex Windows 版本路径。
- Herdr 图标目录：随 `herdr_app::app_home()` 一起切换。
- README 增加 Windows 路径表、构建命令、平台差异与已知限制。

### 4.9 构建与发布

- `Cargo.toml` 新增 Windows 目标依赖：`windows = "0.62"`（与 wgpu 已锁定的 0.62.2 共用，不引入第二个 windows 版本；D2D/DWrite/WinRT/Shell 相关 features 按需裁剪）与 `resvg`（实施时锁定版本）；
- `tokio` 增加 `process` feature；
- 提交 `assets/app-icon.ico`（16–256 多尺寸）；`build.rs` 按 `CARGO_CFG_TARGET_OS` 分支：仅 Windows 目标编译资源，设置 exe 图标、版本信息与 manifest（PerMonitorV2、asInvoker、common-controls v6），macOS 构建直接返回；
- 里程碑构建保留控制台子系统，便于用户看到 `eprintln!` 日志；正式发布使用 `windows_subsystem = "windows"`。若需要发布版可观测性，再加最小日志文件；
- CI 新增 `windows-latest` job：
  - `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`；
  - `cargo build --release`，产出 `target/release/herdr-pet.exe`；
  - 上传 artifact，命名 `herdr-pet-<version>-windows-x64.exe`；
- 发布建议把现有 macOS workflow 改为「macOS + Windows 两个 package job + 一个 publish job」，避免两个 workflow 同时创建/更新 GitHub Release；tag 版本校验、release notes 校验和 `chore: 发布 v<version>` 流程保持不变；
- macOS 本机可用 `cargo check --target x86_64-pc-windows-msvc` 做静态编译检查（需 `rustup target add x86_64-pc-windows-msvc`）；它只验证类型与条件编译，不执行 Windows 资源编译、链接和运行，不能替代 Windows 构建与实机验证。

## 5. 代码改动清单

| 文件 | 改动 |
| --- | --- |
| `src/platform/mod.rs` | 新增 Windows 模块导出；把 `not(macos)` stub 收口为 `not(any(macos, windows))`；共享 `UiState`/`diff`；`work_area`；`set_cursor_hittest` 封装；overlay 位图类型 |
| `src/platform/windows.rs`（新增） | 鼠标/工作区/点击穿透/Herdr 路径/D2D 气泡/Win32 设置面板/托盘/通知/关于 |
| `src/main.rs` | Windows 窗口属性；工作区；overlay 位图上传；业务逻辑不变 |
| `src/render.rs` | `DxgiFromVisual`；Windows alpha mode；overlay 纹理与 pipeline |
| `src/herdr.rs` | 传输抽象；`hc connect` 子进程；泛化读写；测试 |
| `src/herdr_app.rs` | Windows app home / 路径发现 / 前台判断 / 拉起方式 |
| `src/config.rs` | Windows 配置路径 |
| `src/notify.rs` | 平台化通知与提示音 |
| `src/tray.rs` | Windows 彩色托盘图标 |
| `build.rs` / `assets/app-icon.ico` | exe 图标、manifest、版本信息 |
| `.github/workflows/*` | Windows 构建、exe artifact、同一 Release 发布 |
| `README.md` / `docs/design.md` | 平台说明、构建、路径、差异与限制 |

## 6. 里程碑

### M0 技术验证版 + herdr 数据链路

内容：

- `platform/windows.rs` 骨架；
- `DxgiFromVisual` 透明窗口 + 宠物 quad；
- `GetCursorPos` / `GetAsyncKeyState` 拖拽；
- 逐像素点击穿透；
- 彩色托盘图标；
- Windows 传输完整实现：`hc connect`、`hello` / `events.subscribe`、快照、事件、断线重连；
- 状态表接入现有 `Animator`，驱动 working / blocked / done / error 动画（done 的已读细分在 M2 接入 `view.json`）；
- 最小 Windows CI 构建 job（供里程碑验证；Release 发布在 M4 接入），或在 Windows 本机用 `cargo build --release` 产出验证 exe；
- 不实现气泡和设置面板。

→ 验证：通过 Windows 本机构建或 CI 产出 exe，用户在 Windows 实机确认：透明无黑底、始终置顶、无任务栏条目、宠物可拖动、透明像素点击穿透；真实 herdr-core 的 working / blocked / done / error 动画正确，断开/重启 core 后能重连。若透明或点击穿透失败，暂停后续移植，按第 8 节兜底方案调整。

### M1 桌面行为

内容：

- 多显示器、混合 DPI、任务栏工作区避让；
- 位置持久化；
- 整窗点击穿透开关；
- 托盘完整菜单、宠物右键菜单、隐藏托盘图标；
- 配置路径切换。

→ 验证：用户执行验收清单第 1–6 项；宠物跨屏拖动不跳变、不进入任务栏；重启后位置保持。

→ 已通过实机验收（2026-11-25）：点击穿透开关同时放进托盘菜单与宠物右键菜单
（设置面板在 M3）；默认位置改用 `rcWork`，拖动按窗口当前 DPI 计算，配置迁移到
`%APPDATA%`。验收清单第 2、4、5 项已确认；第 3 项（多显示器/混合 DPI）按用户要求
跳过；第 6 项中的设置/关于随 M3 交付。

### M2 Herdr.app 与提醒

内容：

- Herdr 前台判断；
- 点击跳转与子 agent `terminal.peek`；
- `view.json` 已读状态；
- Toast / 提示音与开关。

→ 验证：用户执行验收清单第 9–11 项；用真实 herdr-core/Herdr 确认提醒、前台隐藏、点击跳转；已读 done 在查看后从 review 回坐姿。

→ 已实现并验证（2026-11-25）：前台判断、点击跳转与 `view.json` 已读路径接通；
Toast 在 AppUserModelId 注册后实机弹出（“osc-test2 等待输入”），点击宠物实测拉起
Herdr 并聚焦当前 agent；提示音走 `PlaySoundW`（听感待用户确认）。前台判断的可视
效果（气泡/按钮隐藏）随 M3 气泡一起验收。

### M3 气泡与设置面板

内容：

- Direct2D 气泡位图 + wgpu overlay；
- 文案、Agent 图标、状态图标、spinner、分页箭头；
- 深浅主题、`chat_opacity`、边缘翻转/横移；
- Win32 设置面板、目录选择、失焦关闭；
- 关于对话框。

→ 验证：用户执行验收清单第 7–8 项；确认无真实模糊但半透明填充可读、可点击、可收起。

→ 已实现并完成合成鼠标验证（2026-11-25）：`ChatOverlay` 以 Direct2D DC render
target 渲染预乘 BGRA 位图，wgpu 增加 overlay 纹理/pipeline；气泡文字、Herdr SVG
图标、done/blocked/working 状态、旋转 spinner、分页箭头、展开/收起、长标题省略号
均实机确认；拖到屏幕顶边时气泡翻到下方，窗口 region 随气泡/按钮更新。设置面板
（`herdr_pet_settings`）能即时修改提醒、大小、不透明度与点击穿透并写入配置，目录
选择器正常弹出并可取消，面板外点击关闭；关于显示版本与仓库地址。真实鼠标验收
（第 7–8 项）待用户执行；TaskDialog 与设置面板深色换肤不在 M3 范围。

→ 后续修复（2026-11-25，用户实机遇到后定位）：调整宠物大小（或跨 DPI）会重建
`ID2D1DCRenderTarget`；之前缓存的 Agent 图标 `ID2D1Bitmap` 属于旧目标，下一次
`EndDraw` 返回 `D2DERR_WRONG_RENDER_TARGET`（0x88990015），气泡停止刷新且日志
刷屏。改为只缓存图标栅格像素、每次绘制按当前目标上传位图，并在重建目标时丢弃旧
DPI 下的栅格。旧构建用 `SetWindowPos` 改变窗口尺寸即可稳定复现，修复版同一操作
无报错且气泡/图标正常重绘（CI run 45535315482）。

### M4 发布

内容：

- 正式 `windows_subsystem = "windows"`；
- icon/manifest；
- Windows CI job + 同一 GitHub Release 发布 exe；
- README、`docs/design.md`、release notes 更新；
- SmartScreen 提示。

→ 验证：用户执行验收清单第 12 项；推送 tag 后 CI 校验版本与 `docs/release-notes/v<version>.md`，Release 同时包含 dmg 与 exe；用户在干净 Windows 机器上直接运行单 exe 完成验收。

## 7. Windows 手动验收清单

每个里程碑至少覆盖相关项：

1. 浅色/深色壁纸、100%/150%/200% 缩放下透明边缘无黑底、白边或闪烁。
2. 始终置顶；任务栏无图标；Alt+Tab 无条目；点击宠物不抢焦点。
3. 拖动宠物跟手；跨相邻屏幕无跳变；不进入上/下/左/右任务栏。
4. 宠物位置重启后保持；气泡翻转/收起不移动宠物。
5. 透明像素点击穿透；宠物本体可点击/拖动；整窗点击穿透开关生效。
6. 托盘显示/隐藏、设置、关于、退出；隐藏托盘图标后宠物右键菜单可用。
7. 设置面板：宠物列表、目录选择、大小、不透明度、六个开关；修改立即生效并持久化；点击面板外关闭。
8. 气泡：两行文案、Agent 图标、状态图标、spinner、左右箭头、收起/展开、靠近屏幕边缘翻转/横移。
9. 通知与提示音；`notifications` / `notification_sound` 开关分别生效。
10. Herdr 在前台时聊天 UI 隐藏，切走后恢复；点击气泡/宠物拉起 Herdr 并跳转；子 agent 使用 peek。
11. herdr-core 未启动、运行中重启、断线场景；宠物保留最后状态并自动重连。
12. 单 exe 在干净机器运行；配置写入 `%APPDATA%\herdr-pet\config.json`；SmartScreen 提示可绕过。

## 8. 风险与应对

| 风险 | 影响 | 应对 |
| --- | --- | --- |
| DirectComposition 透明与点击穿透冲突 | 无法实现逐像素穿透或透明失效 | M0 先验证；兜底改为 `WS_EX_LAYERED` + `UpdateLayeredWindow` 渲染宠物与气泡，放弃 Windows 的 wgpu pet 路径 |
| `with_no_redirection_bitmap` 与 `set_cursor_hittest` 不兼容 | 点击穿透异常 | 不改用 winit 的 `set_cursor_hittest`，按 4.2 使用窗口 region 命中掩码 |
| 单 exe 的 Toast 不显示 | 完成/阻塞提醒缺失 | 先实测；必要时首次运行创建带 AUMID 的 Start Menu 快捷方式；再不行补托盘气泡 |
| Herdr Windows 安装路径/CLI 名称不一致 | 前台判断或跳转失败 | PATH + `%LOCALAPPDATA%` + 注册表多路查找；找不到时降级，不影响宠物 |
| 无真实背景模糊 | 视觉与 macOS 不一致 | 首版接受半透明填充，README 标注 |
| 虚拟桌面/独占全屏 | 切桌面后宠物不可见 | Windows 无等价 Spaces API，写入已知限制 |
| 混合 DPI 下坐标偏差 | 宠物位置/凝视偏差 | 统一使用物理坐标；实机多缩放回归 |
| 未签名 exe | SmartScreen 拦截 | release notes 与 README 给出“更多信息 → 仍要运行”；后续可选代码签名 |
| RenderDoc 不支持 DirectComposition | 调试工具受限 | 开发期可用日志/截图，发布不影响 |

## 9. 发布方案

- 产物：`herdr-pet-<version>-windows-x64.exe`，单文件、无安装器、无 zip；
- 与 macOS dmg 发布到同一个 GitHub Release；
- `docs/release-notes/v<version>.md` 必须新增 Windows 分节，保留既有 DMG 签名提示，并补充 SmartScreen 提示；
- 版本号同步 `Cargo.toml`、`Cargo.lock` 与 README 中的 tag 示例，提交 `chore: 发布 v<version>` 后推送同名 tag；
- 首版不签名；若后续提供证书，再在 CI 中增加签名步骤。

## 10. 下一步

1. ~~M0~~：已完成并在 Windows 实机跑通；
2. ~~M1~~：已通过实机验收（多显示器按用户要求跳过）；
3. ~~M2~~：已实现；Toast 与点击跳转已实机验证，提示音待听感确认；
4. ~~M3~~：已实现并完成合成鼠标验证；第 7–8 项真实鼠标验收待用户执行；
5. 下一步 M4：发布（`windows_subsystem`、exe 图标与 manifest、Windows CI 发布
   job、README/design/release notes），manifest 落地后把“关于”升级为 TaskDialog。

实施过程中在 Windows 实机确认以下未定项，并回填本文：Herdr app home、`Herdr.exe` / `herdr-app.exe` 实际路径、Toast 是否需要 Start Menu 快捷方式、任务栏在多显示器下的 `rcWork` 行为。M3 另确认：Direct2D DC render target 与
DirectComposition 共存正常；`WS_EX_TOOLWINDOW` 设置窗口 + 跨线程 owner 的
`IFileOpenDialog` 能正常置于置顶面板之上；`GetDpiForWindow` 在 200% 缩放下返回 192。
