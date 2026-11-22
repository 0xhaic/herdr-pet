# herdr-pet 设计方案

> 当前状态：M0–M6 已实现（M6 含实机回归与文档校准）；长待机随机动作、坐姿默认与
> 悬停跳动已实现并实机验证；透明合成改为预乘 alpha，浅色背景白边已修复并实机验证。
> v0.1.2：应用图标与托盘图标已按「探头」方向落地（2026-11）；`.app` 携带 `AppIcon.icns`，
> 菜单栏使用与应用图标同轮廓的方形 alpha 模板图（圆角底板实心、猫头镂空）。
> v0.1.3：修正 status 事件解码（根 7501 记录在 `data.status` 下），状态变化会立即驱动
> 动画并在 blocked/done 变化沿发送通知；通知改为应用内原生发送，横幅图标与 `AppIcon`
> 统一（无 bundle 身份时回落 osascript）。`cargo test`（74 项）、`cargo build`、
> `cargo clippy --all-targets -- -D warnings` 全绿，已用真实 herdr-core 0.1.21 实机验证。
> v0.1.4：透明合成改在 sRGB 编码空间预乘（CPU 预乘 + 非 sRGB atlas/swapchain），
> 修复浅色背景下残留的宠物白边。`cargo test`（75 项）、`cargo build`、
> `cargo clippy --all-targets -- -D warnings` 全绿，已实机 A/B 验证。
> v0.1.5：新增托盘「关于」菜单（原生 NSAlert：应用 logo、编译期版本号与仓库链接），
> 仓库采用 MIT 协议。`cargo test`（75 项）、`cargo build`、
> `cargo clippy --all-targets -- -D warnings` 全绿，已打包 `.app` 实机验证对话框。
> v0.1.6：重绘改为仅在可见帧变化时 present（60 Hz tick 只推进状态；窗口隐藏时
> 250 ms tick 且不渲染）；done 的 jumping 播放结束后，坐姿也支持鼠标凝视
> （离开凝视回坐姿，不重播 jumping）。`cargo test`（79 项）、`cargo build`、
> `cargo clippy --all-targets -- -D warnings` 全绿，已用假 herdr + 安装后的
> release 二进制实机验证。
> v0.1.7：done 坐姿的凝视改为限时窗口——进入 done 后前 30 秒只坐姿，随后开放
> 60 秒凝视窗口（第 30–90 秒），窗口结束回到 idle 坐姿；计时按聚合状态连续保持
> done 计算，Done 中断会重置。`cargo test`（80 项）、`cargo build`、
> `cargo clippy --all-targets -- -D warnings` 全绿，已重新打包覆盖安装。
> v0.1.9：新增宠物上方两行毛玻璃聊天框与右下角收放按钮；仅当 Herdr 不在前台
> （或未启动）且存在 blocked/working/error 或未读 done 时显示；气泡内容按
> blocked/error → 未读 done → 最近状态变化的 working → 其它排序；未读 done
> 查看后消失；点击气泡或宠物拉起 Herdr 并跳转（见 3.10/3.11）。`cargo test`
> （96 项）、`cargo build`、`cargo clippy --all-targets -- -D warnings` 全绿，
> 已实机验证毛玻璃、实时文案、展开/收起、前台隐藏、error 保留、done 未读保留/
> 已读消失、点击跳转、拖动不跳转与位置持久化。

Rust 桌面宠物：复刻 Codex 桌面版的宠物表现，宠物状态来自 herdr-core。

## 1. 目标与范围

### 目标

- 一个独立运行的 macOS 桌面宠物：透明、无边框、置顶、可拖拽、点击透明区域穿透。
- 读取 `~/.codex/pets/<name>/` 下的 Codex 宠物（`pet.json` + `spritesheet.webp`），不改格式、不内置素材。
- 通过 herdr-core 的 Unix socket JSON-RPC 订阅 agent 状态，驱动宠物动画。
- 汇总全部 agent：一只宠物反映当前最紧急的状态，并在 blocked/done 时提醒。

### 已确认决策

| 决策点 | 选择 |
| --- | --- |
| 形态 | 独立桌面窗口（非 herdr 终端内浮层） |
| 状态来源 | 汇总全部 agent |
| GUI | `winit` + `wgpu` |
| 功能 | 基础交互、鼠标凝视（v2 look rows）、提醒与音效 |
| 不做 | 点击养成/亲密度、herdr pane 内浮层、宠物生成（hatch-pet） |

### 非目标

- 不做多平台适配；首版仅 macOS，平台代码隔离，后续可扩展。
- 不下载、不分发 Codex 官方宠物素材（授权限制）。

## 2. 总体架构

```text
herdr-core (Unix socket, JSON-RPC)
        │  hello / events.subscribe
        ▼
  herdr.rs（后台 tokio 任务）
        │  Snapshot / Event / Disconnected
        ▼
  status.rs（终端表 + 聚合 + 提醒判定）
        │  Aggregate / Notification
        ▼
  main.rs（winit 主线程，60 Hz tick）
        ├── pet.rs    宠物加载 / 动画状态机 / 凝视
        ├── chat.rs   聊天框布局、文案与焦点选择（纯逻辑）
        ├── herdr_app.rs  Herdr 点击跳转（前台判断在主线程）
        ├── render.rs 单张 atlas 纹理 + 宠物子矩形 quad
        ├── platform/macos.rs  全局鼠标、拖拽、点击穿透、NSVisualEffectView 聊天框
        ├── notify.rs  afplay + 系统通知（osascript 回落）
        └── tray.rs    tray-icon 菜单
```

线程模型：

- winit `EventLoop` 在主线程，渲染与交互都在主线程。
- tokio runtime 在后台线程，只负责 socket 读写；通过 `std::sync::mpsc` 把
  `Snapshot` / `Event` / `Disconnected` 交给主线程，主线程每帧 drain 一次。
- 主线程任何路径都不得阻塞等待网络或图片解码；只有启动/切换宠物时同步解码一次
  atlas（<200 ms），解码失败保留当前宠物。

依赖固定为已写入 `Cargo.toml` 的版本：`winit 0.30`、`wgpu 30`、`image 0.25`、
`tokio 1.54`、`tray-icon 0.26`、`muda 0.21`、`objc2 0.6` / `objc2-app-kit 0.3`。

## 3. 模块设计

### 3.1 `pet.rs` — 宠物格式与动画（纯逻辑，可单测）

常量：`CELL_W = 192`、`CELL_H = 208`、`ATLAS_COLS = 8`。

加载流程：

1. 读 `pet.json`，字段 `id`、`displayName`、`description`、
   `spriteVersionNumber`（默认 1）、`spritesheetPath`（默认 `spritesheet.webp`）。
2. 解析 spritesheet 路径；拒绝绝对路径和越出宠物目录的路径。
3. 文件大小上限 20 MiB（与 Codex 客户端一致），再解码为 RGBA。
4. 校验：宽必须 `1536`；高必须是 `208` 的整数倍且至少 9 行（1872）。
   高 ≥ 11 行（2288）才启用凝视行 9–10；不足时禁用凝视并记录警告。
5. 保存整张 RGBA atlas；按行/列切 UV，不在内存里展开成多张图。

动画行与用途：

| Row | 名称 | 帧数 | 用途 |
| --- | --- | --- | --- |
| 0 | idle | 6 | 待机/打盹 |
| 1 | running-right | 8 | 向右拖动 |
| 2 | running-left | 8 | 向左拖动 |
| 3 | waving | 4 | blocked 的入场动作 |
| 4 | jumping | 5 | done 的入场动作 |
| 5 | failed | 8 | error/崩溃 |
| 6 | waiting | 6 | blocked 等待输入 |
| 7 | running | 6 | working（打字/处理） |
| 8 | review | 6 | 素材保留；当前状态机不使用 |
| 9 | look A | 8 | 凝视 000°–157.5° |
| 10 | look B | 8 | 凝视 180°–337.5° |

帧时长（ms）：

- idle：`[1680, 660, 660, 840, 840, 1920]`（采用参考实现 herdr-pet 使用的 6.6 s
  平静循环；hatch-pet 文档里的 1.1 s 是素材生成规范，不作为运行时节奏）。
- running-right/left：`[120 × 7, 220]`
- waving：`[140, 140, 140, 280]`
- jumping：`[140, 140, 140, 140, 280]`
- failed：`[140 × 7, 240]`
- waiting：`[150 × 5, 260]`
- running：`[120 × 5, 220]`
- 凝视行由鼠标角度直接选列，不按时间播放。

来源说明：动作行命名与状态→动作映射沿用 Codex 契约（hatch-pet 文档与
herdr-pet README）；除 idle 外各行时长取自 hatch-pet `animation-rows.md`；
idle 时长取自 herdr-pet `src/sprites.rs`。后续若与真实 Codex 行为冲突，以实测为准。

状态机：

```text
Mood::Working  -> running（循环）
Mood::Blocked  -> waving（一次） -> waiting（循环）
Mood::Done     -> jumping（一次）-> 坐姿；连续 done 30 秒后开放 60 秒凝视窗口
Mood::Failed   -> failed（循环）
Mood::Idle     -> idle（循环）-> 长待机随机 waving / jumping（一次）-> idle
拖动中         -> running-right / running-left（覆盖 Mood）
Idle / Done 凝视窗口 + 凝视 -> look row 9/10（仅当 atlas 有 11 行）
鼠标悬停       -> jumping ×3 -> idle（覆盖除拖拽外的状态）
```

- 一次性行播完用余量时间进入下一行，不丢帧。
- 行切换以 `row_started_ms` 为基准；Mood 改变时重置。
- 长待机随机动作是本项目增强，不是 Codex 公开契约：只在 Mood 持续为 `Idle` 时，
  每段连续 idle 满 15–30 秒后随机选择 waving 或 jumping，播完回到 idle。
  15–30 秒包含两端；随机源用可注入种子的轻量 PRNG，不新增依赖。
  其他 Mood 不用随机动作覆盖，以免遮蔽工作状态。idle 计时只在 Base + Idle 状态下推进；
  拖拽、凝视或悬停打断当前 idle/随机动作，返回时从头播放当前 Mood 的入场动作并重置 idle 计时
  （Done 坐姿的凝视例外：返回坐姿，不重播 jumping）。
- 凝视：`angle = atan2(dx, -dy)`，0° 向上，顺时针每 22.5° 一格，共 16 格；
  距离小于死区（24 logical px）回落到 idle。鼠标在宠物正上方时 `000`。
  Done 的 jumping 播放结束后进入坐姿；从首次观察到聚合状态连续为 Done 起，
  前 30 秒不响应凝视，之后开放 60 秒凝视窗口（第 30–90 秒）。窗口内凝视结束
  回到坐姿并重置 idle 计时，不重播 jumping；窗口在第 90 秒关闭时，即使鼠标仍在
  凝视区也回到 idle 坐姿，且本次 Done 不再凝视；jumping 入场本身不会被凝视打断。
  计时只随聚合状态是否连续为 Done 推进：拖拽、悬停不会暂停或重置；聚合状态离开
  Done 后再次进入时，重新从 30 秒延迟开始。
- 悬停：与 Codex 宠物组件的 `respondToHover` 一致。鼠标进入宠物窗口矩形即悬停，
  播放 jumping 3 次后进入 idle 坐姿循环；悬停期间覆盖除拖拽外的状态并暂停凝视，
  移出后恢复当前 Mood（重置 idle 计时）。
- Done 不再循环 review：herdr 的 done 会长期存在，review 会让宠物一直站立；
  jumping 播完回到 idle 坐姿，保留变化沿提醒但不阻塞默认坐姿。
- 测试锚点：`frame_index` 边界、waving→waiting、jumping→idle、idle 随机动作的
  阈值/选择/回退/打断与重置、悬停 jumping×3→idle、悬停覆盖凝视、拖拽优先、
  Done 凝视的 30 秒延迟、60 秒窗口开闭与 Done 中断重置、jumping 不被凝视跳过、
  离开凝视回坐姿、8 个基本方向、16 格取整、死区、无凝视行时回落 idle。

### 3.2 `status.rs` — agent 状态表与聚合（纯逻辑，可单测）

按 `Terminal.address` 维护：

```text
TerminalAgent { address, title, name, labels, live, status: Option<AgentStatus>, status_ms }
AgentStatus   { Idle, Done, Working, Error, Blocked }   // Ord 即显示优先级
```

- 只统计 `live == true` 且 `status.is_some()` 的终端；普通 shell 不计入。
- 聚合优先级：`Blocked > Error > Working > Done > Idle`。
  （herdr 自身是 `blocked > done > working > idle`；error 插在 blocked 之后，
  保证"崩溃"能被看到。Working 提到 Done 之前，避免长期存在的 done 终端压住
  仍在执行的任务——站立只表示有任务在执行。）
- 输出 `Aggregate { status: Option<AgentStatus>, blocked, error, done, working, total }`。
- 聊天框的聚焦 agent（`focus_with`）：只在 `live` 且 `status.is_some()` 的终端中
  选，分组为 1) blocked/error、2) 未读 done、3) 最近一次状态变化的 working、
  4) 其它；同一组取最早状态时间（working 组例外，取最近）。`status_ms` 来自快照；
  状态事件使用接收时的墙上时钟，用于同一组的时间顺序。`Status` 记录的 `msg`
  随 `TerminalAgent` 保存为 `status_msg`；status 更新时同步，status 清除、
  finished/lost 时清空，避免第二行残留旧动作。
- 气泡聚焦与宠物动画分开：动画仍用 `Aggregate.status` 的
  `Blocked > Error > Working > Done > Idle` 聚合；气泡用上面的分组，两者可能指向
  不同 agent（例如 bubble 选更早的 error，动画按 aggregate 选 blocked）。
- `is_active_with(unseen_done)`：blocked / working / error 始终算活跃；done 取决于
  回调，Herdr 未读时算活跃，已读后不算；idle 与无状态终端不算。聊天 UI 的可用性
  用这个逐终端判断，而不是 `Aggregate` 计数。
- 通知规则：
  - 只在状态的**变化沿**提醒：进入 `Blocked` 或 `Done`（`old != new`）。
  - 初始 snapshot 不提醒；`created` 后紧接的状态变化正常提醒。
  - 主线程把同一帧内多条提醒合并为一条（"N 个 agent 等待输入"）。
  - 文本优先级：`title` → `name` → `labels["where"]` → `address`。
- 计数单位是 terminal address：父 agent 与子 agent（`parent` 非空）各算一个。
  herdr 的 agents 列表同样包含 sub-agent，因此聚合不做父子去重，只按地址去重。
  单测锚点：父、子同时 blocked 时 `blocked == 2`。
- herdr 语义：`done` 表示"你没看的时候完成了"；任一 Herdr 客户端查看后会转为
  `idle`。pet 只是事实订阅者，订阅是否触发该转换尚未实测，因此不要把 done→idle
  当作必然事件；宠物播放一次 jumping 提醒后回到 idle 坐姿，等待下一次状态变化。

### 3.3 `herdr.rs` — JSON-RPC 客户端

Socket 发现顺序：

1. `HC_TERMINAL_SOCKET`（在 herdr 终端内启动时必然存在，本机已验证）。
2. `$XDG_STATE_HOME/herdr-core/<session>/<session>.sock`
   （默认 `~/.local/state/herdr-core/<session>/<session>.sock`）。
   `<session>` 取 `HC_SESSION`，否则 `default`。

协议（JSON-RPC 2.0，一行一条消息，generation 1）：

1. `hello`，params `{"generation":1}`；读到 `id=1` 的响应为止。
2. `events.subscribe`，params `{}`；响应 `{ seq, terminals }` 即初始快照。
3. 之后服务端推 `method:"event"` 通知：

```json
{"jsonrpc":"2.0","method":"event","params":{
  "type":"status","terminal":"t20","seq":2099,
  "data":{"records":{},"reported":true,
          "status":{"state":"blocked","app":"claude-code","msg":"..."}}}}
```

`params.type` 至少处理：`created`、`status`、`finished`、`lost`、`removed`、
`title`、`labels`、`turn`、`missed`。`heartbeat` 忽略。

- `created` 的 `data` 是完整 Terminal；`status` 的 `data` 是更新包，根 7501
  记录在 `data.status`（`records` 是其它记录）；`finished` 的 `data` 含
  `exit_code/finished_ms/reason/signal`；`title` 的 `data` 是 `{"title": ...}`；
  `labels` 的 `data` 是 `{"labels": {...}}`；`removed` 的 `data` 为空对象。
  `status` 更新包的 `data.status` 为 `null` 表示程序已清除状态（root shell
  回到前台，例如 agent 退出后的普通 zsh/bash 终端），终端按 idle 处理；
  `data.status` 缺失或 `state` 为空时按未知事件忽略，不得覆盖终端现有状态。
- 协议以 herdr-core 0.1.21 实测为准（2026-11-19，直连 socket 抓包）；
  其它事件类型（`foreground`、`size`、`doc` 等）忽略。
- `missed`：直接断开重连，靠新订阅拿新快照；不尝试补事件。
- 读超时 45 s（服务端每 15 s 心跳）；超时/断开后 2 s 退避重连。
- 重连期间 UI 保持最后状态；`Disconnected` 只用于日志，不清空宠物状态。
- 不做轮询；初始快照之外不再调用 `terminal.list`。
- 可单测：用临时 Unix socket 起一个假服务端，验证握手、快照、事件、断开重连。

### 3.4 `render.rs` — wgpu 渲染

- 一张 atlas 纹理（RGBA，非 sRGB），一个 quad，uniform 含 UV 矩形与裁剪空间
  子矩形 `[left, top, right, bottom]`；宠物只画在窗口内的宠物格子里，其余区域保持
  全透明，供原生毛玻璃浮层合成。
- 窗口服务器把非不透明 CAMetalLayer 当作 **sRGB 编码空间中的预乘 alpha**
  合成。因此 atlas 在上传前按 `rgb' = round(rgb * a)` 在编码空间预乘，
  纹理用 `Rgba8Unorm`；surface 选非 sRGB 格式，片元着色器原样输出采样值，
  quad 覆盖整个窗口，混合用 `REPLACE`；clear 为全透明。不要在 shader 里
  先做 sRGB 解码再乘 alpha：那会让每个软边缘比正确的预乘值更亮，浅色背景
  上表现为白边（2026-11-19 实机验证与数值模拟）。
- `alpha_mode` 用 `PostMultiplied`：wgpu 30 的 Metal 后端仅在
  `PostMultiplied` 时把 CAMetalLayer 设为非不透明；`PreMultiplied` 会得到
  黑底（2026-11-19 实机验证）。该模式只改 opaque 标记，窗口服务器仍按预乘
  alpha 合成。
- 采样器线性过滤（非像素风宠物也好看），过滤发生在预乘后的编码值上，
  不会从全透明像素里带出边缘杂色；纹理坐标 clamp。
- 60 Hz tick 只推进输入与动画状态；只有当前单元格（UV）变化、首次显示、
  切换宠物、窗口 resize、窗口重新显示或渲染报错时才请求重绘并 present。
  窗口隐藏时 tick 降到 250 ms 且不渲染。
- 切换宠物时重建纹理与 bind group；失败则保留旧纹理。

### 3.5 `platform/macos.rs` — 窗口交互

- 窗口：`with_decorations(false)`、`with_transparent(true)`、
  `with_resizable(false)`、`WindowLevel::AlwaysOnTop`、
  `ActivationPolicy::Accessory`（不显示 Dock 图标）。
- 默认尺寸：宠物高度 104 logical px（Retina 上正好 208 物理像素 = 原生分辨率）。
  窗口固定 336 logical px 宽（320 气泡 + 左右各 8 留白），高度为
  `8 + 64 气泡 + 16 间距 + 宠物高 + 10 + 28 按钮 + 8`；宠物居中，气泡在宠物上方，
  箭头按钮在宠物下方。位置默认主屏右下角，距边缘 24 px，可被拖拽覆盖并持久化。
- 点击穿透：默认按像素判定——每帧读取全局鼠标位置，换算到窗口内 point 坐标；
  宠物格内采样当前帧 alpha（alpha < 16 穿透），箭头按钮与展开状态的气泡矩形直接
  可点击，其余区域穿透。`window.set_cursor_hittest` 在 winit 0.30 上映射到
  `setIgnoresMouseEvents:`。
- 点击归属：箭头按钮只切换聊天框（不拖拽）；点击气泡拉起 Herdr 并跳到当前会话；
  宠物格内按下先进入拖拽候选，释放时位移 ≤ 3 logical px 才视为点击并跳转，超过则
  只拖动窗口。聊天框隐藏时按钮/气泡矩形不再可点击或穿透拦截，但宠物点击仍可跳转。
  拖动、悬停与凝视都以宠物格中心为基准，不受更高窗口影响。
  `config.position` 持久化宠物左上角而非窗口左上角，收起/展开不移动宠物。
- 整窗穿透开关（`config.click_through`）：开启后窗口始终
  `set_cursor_hittest(false)`，鼠标事件全部落到桌面，宠物不可拖拽；
  关闭后恢复上述逐像素判定。只能通过托盘菜单切换；切换时立即生效并持久化，
  若正在拖拽则结束拖拽。
- Space/全屏：窗口设置 `NSWindowCollectionBehavior`
  （`CanJoinAllSpaces | FullScreenAuxiliary | Stationary`），
  在桌面与其它应用的全屏 Space 中都保持可见；切换 Space 或进出全屏应用后
  宠物仍悬浮在当前界面，不会被留在其它 Space。
- 拖拽：在宠物不透明区域按下左键开始；按住期间轮询全局坐标与
  `NSEvent::pressedMouseButtons`，用位移移动窗口；释放时保存位置。
  不依赖辅助功能权限，不需要全局事件 tap。拖拽期间播放 running-left/right。
- 凝视：同一套全局坐标，相对窗口中心计算方向；拖拽中禁用。
- 坐标：`NSEvent::mouseLocation` 是以主屏左下角为原点的全局坐标；
  统一换算到"左上角原点、物理像素"后再使用，兼容多屏与 Retina。
- 已知限制（首版接受，后续再修）：多屏/混合缩放下的全局坐标换算见第 7 节。
  在 Accessory 策略下实测点击宠物不会激活 App；若系统行为变化再改
  NSPanel/nonactivating。托盘/菜单栏交互已实测可用。

### 3.6 `notify.rs` — 提醒

- 声音：`afplay /System/Library/Sounds/{Basso|Glass}.aiff`，子进程即发即忘。
- 通知：在 `.app` 内运行时通过 `platform/macos.rs` 的 `NSUserNotificationCenter`
  发送，通知身份就是应用本身，横幅显示 `AppIcon` 与 `herdr-pet` 名称，与
  应用图标统一；首次发送时系统会请求通知权限（一次性）。
- 无 bundle 身份时（`cargo run` 直接运行裸二进制、或非 macOS）回落到
  `osascript -e 'display notification "..." with title "herdr-pet"'`；
  该路径的图标由系统按 Script Editor 显示。
- osascript 回落路径中所有外部文本做 AppleScript 字符串转义（反斜杠、双引号、
  换行），绝不拼接 shell 命令；参数用数组传递。
- `config.notifications = false` 时不发送系统通知，也不播放提醒音效。
- `config.notification_sound = false` 时只关闭提醒音效，系统通知照常发送。

### 3.7 `tray.rs` — 菜单栏

- `tray-icon` + `muda`。图标为内置方形模板图 `assets/tray-icon.png`
  （22 pt @2x，44×44 RGBA，`include_bytes!` 嵌入二进制），与宠物素材无关；
  macOS 上按模板图（`with_icon_templated`）渲染，只取 alpha，由系统按菜单栏明暗反色。
  图形与应用图标共用同一圆角方形底板与猫头几何：底板实心，猫头与双耳为镂空负空间，
  眼睛由底板透出；切换宠物不更新托盘图标。
- 菜单：显示/隐藏、下一只宠物、放大、缩小、点击穿透开关、提醒开关、通知音效开关、
  关于、退出。
- 「关于」用原生 `NSAlert` 显示简介、编译期版本号（`CARGO_PKG_VERSION`，唯一来源是
  `Cargo.toml`）与仓库链接；「打开仓库」用 `NSWorkspace` 打开
  `https://github.com/0xhaic/herdr-pet`。模态期间事件循环暂停（用户主动操作，可接受）。
- 点击对话框外的任意位置即关闭对话框：模态期间注册本地 + 全局 `mouseDown` 监听，
  点击落在对话框窗口内则放行，落在其他位置（其他应用、桌面、宠物窗口、菜单栏）则
  `abortModal()`，随后移除监听。其他应用的点击只有全局监听可见，本应用的点击
  （宠物窗口、菜单栏）只有本地监听可见，因此两者都需要。
- 菜单事件在每帧 tick 中非阻塞 `try_recv`。
- 若 tray-icon 与 winit 集成失败：降级为"无托盘 + 键盘快捷键退出"，
  并在 `docs/design.md` 记录；不引入第二套 GUI 框架。

### 3.8 `config.rs` — 配置

路径：`$XDG_CONFIG_HOME/herdr-pet/config.json`，未设置时
`~/.config/herdr-pet/config.json`。macOS 上也不用 `dirs::config_dir()`
（它会指向 `~/Library/Application Support`），保持与文档一致的显式路径。

```json
{
  "pet": "diana",
  "size": 104,
  "position": [1512, 812],
  "notifications": true,
  "notification_sound": true,
  "gaze": true,
  "click_through": false,
  "chat_open": true,
  "chat_opacity": 0.7
}
```

- `pet`：名字或目录路径；空/缺失时取搜索目录中第一个可加载宠物。
- `position`：宠物左上角，物理像素、左上角原点；启动时换算成窗口位置并通过可见性
  检查，窗口完全在所有屏幕之外则回落到默认位置。这样旧配置（窗口即宠物）升级后宠物
  位置不变，展开/收起气泡也不会移动宠物。
- `notifications` / `notification_sound`：分别控制系统通知与提醒音效，均默认 `true`；
  只关闭音效时仍发送通知，关闭提醒时两者都不执行。
- `click_through`：整窗点击穿透开关，默认 `false`（保持逐像素穿透 + 可拖拽）；
  开启后鼠标完全穿透到桌面，只能通过托盘关闭。
- `chat_open`：聊天框展开状态，默认 `true`；点击宠物下方箭头按钮切换并写回。
- `chat_opacity`：聊天气泡玻璃层的不透明度（0–1，越大越不透明），默认 `0.7`，非有限值
  取默认；`0` 时玻璃层与描边完全透明、只显示文字。只在启动时读取，修改后重启生效。
- 写入采用"临时文件 + rename"原子替换；写入失败只记日志，不阻塞 UI。

### 3.9 图标资产 — `.app` 图标与托盘剪影

- 选定方向：02「探头」——暖奶油圆角底板、陶土色桌沿、墨色猫头，两只前爪搭在边上。
- 应用图标：1024×1024 源图，824 px 连续圆角底板，四周 100 px 透明边；源文件
  `assets/app-icon.svg`，导出 `assets/app-icon.png`（1024 RGBA）。图标内容裁在底板内，
  透明边保持干净。
- 打包用 `packaging/AppIcon.icns`（由 iconset 生成，含 16–1024 各档）；`Info.plist`
  设置 `CFBundleIconFile=AppIcon`，`scripts/bundle.sh` 将其复制到
  `Contents/Resources/AppIcon.icns`。应用以 `LSUIElement` 运行，不在 Dock 显示；图标
  用于 Finder、DMG 与通知。
- 托盘：`assets/tray-icon.png` 为 44×44 RGBA 黑/透明模板图，直接复用应用图标的
  1024 坐标（等比缩放到 44 px，底板约 35 px 并居中留白）；圆角底板实心，猫头与双耳
  镂空为负空间，眼睛由底板透出；只取 alpha，由系统按明暗反色。源文件
  `assets/tray-icon.svg`，与应用图标分开维护。
- 图标源文件与应用代码分开：SVG 是唯一可编辑源，PNG/ICNS 是导出产物；不引入生成式
  位图或额外的图标构建依赖。
- 三方向探索稿与尺寸/托盘测试页保留在本地 `design/logo-preview.html`，仅作本地开发参照，
  不随仓库分发（已加入 `.gitignore`），不参与打包。

### 3.10 `chat.rs` + AppKit 浮层 — 聊天框

布局（logical px，窗口左上原点）：

| 元素 | 几何 |
| --- | --- |
| 窗口宽 | 336 = 320 气泡 + 左右各 8 留白 |
| 气泡 | 320 × 64，圆角 32，位于顶部，左右居中 |
| 宠物格 | 192/208 比例的 cell，水平居中，顶部距气泡底 16 |
| 箭头按钮 | 28 × 28 圆形，宠物格下方 6，右边缘与宠物格右边缘对齐 |

- 窗口高度 = `8 + 64 + 16 + 宠物高 + 6 + 28 + 8`；收起只 `setHidden(true)` 气泡，
  不改变窗口尺寸或位置，宠物不会跳动。
- 气泡用 `NSVisualEffectView`（`HUDWindow`、`BehindWindow`、`Active`）提供真实背景
  模糊，不叠加填充底色，整体 `alphaValue` 取 `config.chat_opacity`（默认 0.7），仅保留
  1px 白色低透明度描边；两行 `NSTextField` 13 pt，统一用不透明的 `textColor` 语义色
  （标题粗体、副标题常规并以视图 alpha 0.85 弱化），随系统浅色/深色外观切换为黑/白字，
  避免浅色玻璃上的白字不可读；固定 16 pt 行高，单行尾部省略。
  不用第二套 GUI 框架，也不在 wgpu 里伪造模糊。
- 箭头按钮用同材质的圆形毛玻璃，chevron.down（展开）/ chevron.up（收起）为
  SF Symbol。
- 浮层挂在 winit 内容视图的透明容器的子视图上；容器 `hitTest:` 返回 nil，点击
  交回 winit，由 3.5 的矩形判定处理，避免 AppKit 视图吞掉按钮点击。
- 文案：第一行 = 聚焦 agent 的 `notification_text()`（title → name → where →
  address）；第二行 = `status.msg`（程序自己的动作文案），为空时回退
  `空闲 / 工作中 / 等待输入 / 已完成 / 出错了`。没有带状态的 agent 时显示
  `暂无任务 / 等待 agent 状态`。文本折叠空白并截断到 120 字符，避免异常长度。
- 状态：`config.chat_open` 默认 true；点击箭头切换并写回，不新增托盘菜单项。
- 可用性：`chat_ui = active && !herdr_frontmost`。`active` 指任一 live agent 处于
  `blocked` / `working` / `error`，或处于 `done` 且 Herdr 尚未标记已读；
  `herdr_frontmost` 由 NSWorkspace 判断，Herdr 未启动时为 false。两者共同决定：
  仅当存在活跃 agent 且 Herdr 不在前台时可用。不可用时气泡和按钮都
  `setHidden(true)`，两者矩形退出逐像素命中（点击穿透）；宠物本体保留，点击宠物
  仍会拉起 Herdr。可用时按钮显示，气泡按 `config.chat_open` 决定；Herdr 在前台时
  无法展开/收起。
- done 未读：读取 Herdr 的 `view.json` → `attention[address].seen`。seen=false 或
  没有该终端记录时视为未读；Herdr 查看后 seen=true，下一次 500 ms 轮询即不再算
  活跃。文件不可读时保持全部 done 未读（气泡保留），避免静默丢完成提醒。
- 前台判断在主线程每 250 ms 调一次 `NSWorkspace.frontmostApplication`，不启动
  `herdr-app status` 子进程，也不区分 Herdr 当前展示哪个 tab。
- 渲染：宠物 quad 只占布局中的宠物格；子矩形由 App 每帧换算成裁剪空间坐标传给
  shader，窗口其余区域保持透明。悬停、凝视中心与逐像素穿透都改用宠物格坐标。
- 测试锚点：布局尺寸/居中/随高度增长、矩形半开区间、气泡聚焦优先级与同组时间
  顺序、`msg` 跟随 status 更新/清除、单行折叠与截断。

### 3.11 `herdr_app.rs` — 已读状态与点击跳转

Herdr.app 负责窗口与布局，herdr-core 负责终端。可见性依赖 3.10 的
`NSWorkspace` 前台判断和本模块的已读状态；本模块还负责点击后的窗口跳转，不碰
herdr-core 的事件流，也不停止/重启用户的 Herdr 或 core。

- 已读轮询：后台线程每 500 ms 读取 Herdr 的 `view.json`，只取
  `attention[*].seen` 为 true 的终端地址集合；读/解析失败时保留上一次成功结果
  （首次失败为空集 = 全部未读）。路径解析顺序与 Herdr 一致：
  `HERDR_APP_HOME` → `$XDG_CONFIG_HOME/herdr-app` → `~/.config/herdr-app`。
  文件约 20 KB，无子进程。
- 该文件是 Herdr 的内部视图状态（无公开 CLI 提供逐终端 seen）；Herdr 未运行或
  文件不可读时 done 一直按未读处理。

- 点击跳转：点气泡（按下即触发）或点宠物（释放且位移 ≤ 3 logical px）时，
  捕获当前聚焦终端地址，后台线程先 `/usr/bin/open -b dev.herdr.app` 拉起/激活窗口，
  再重试 `herdr-app do agent.focus --terminal <地址>`（25 × 200 ms，覆盖冷启动）。
  无聚焦 agent 时只拉起窗口。点击期间用原子标记避免重复跳转线程。
- 应用路径通过 `NSWorkspace.URLForApplicationWithBundleIdentifier:` 定位
  `dev.herdr.app` 的可执行文件（兼作 `herdr-app` CLI）；找不到时无法跳转，宠物
  其余功能不受影响。
- 不再轮询 `herdr-app status`，也不解析 space/tab/agent；Herdr 前台时整个聊天 UI
  隐藏，与当前打开哪个 tab 无关。
- 测试锚点：attention 的 seen 地址解析、`is_active_with` 的 done 分支、点击阈值
  与可见性组合逻辑；子进程跳转路径不做单测。

## 4. 错误处理与健壮性

- 边界用 `anyhow::Result`；运行时路径禁止 `unwrap()`（测试除外）。
- pet.json / spritesheet 视为不可信输入：限制大小、校验尺寸、拒绝路径穿越。
- 图片解码失败、socket 断开、配置损坏都不得让进程退出；记录并降级。
- 通知文本不进入 shell；AppleScript 单独转义。
- 不把用户的主目录、agent 标题等隐私信息发送到任何外部服务。

## 5. 测试策略

单元测试（`cargo test`）：

- `pet`：manifest 解析/默认值、尺寸校验、`frame_index` 边界、
  一次性行动画接续、长待机随机动作、凝视角度与死区、11 行以下禁用凝视。
- `status`：聚合优先级、初始快照不提醒、进入 blocked/done 提醒、
  finished/lost 排除、真实事件 JSON 解析、气泡聚焦优先级与同组时间顺序、
  `msg` 更新/清除、blocked/working/error 与未读 done 视为活跃。
- `chat`：三段布局尺寸与居中、随宠物高度增长、矩形命中、两行文案回退、
  单行折叠与截断。
- `herdr_app`：`view.json` attention 的 seen 地址解析。
- `main`：点击 vs 拖动阈值、聊天 UI 可见性（活跃 + Herdr 非前台）、`chat_opacity`
  越界/非法值回落。
- `herdr`：临时 Unix socket 假服务端，验证 hello → subscribe → 事件 → 重连。
- `config`：默认值、round-trip、损坏文件回落。
- `notify`：AppleScript 转义。

集成/手工验证：

1. `cargo test`、`cargo build`、`cargo clippy --all-targets -- -D warnings`。
2. `cargo run` 后截图确认透明、置顶、无 Dock 图标。
3. 拖动宠物并重启，确认位置保留。
4. 用真实 herdr agent 触发 working / blocked / done，确认动画与提醒。
5. 鼠标绕宠物移动，确认 16 方向凝视；移入死区回到 idle。
6. 连续 idle 15–30 秒，确认随机 waving/jumping 播放一次后回 idle；随机动作期间
   移入鼠标触发凝视，离开后 idle 重新计时。截图确认播放中无黑底、闪烁或越界。
7. 聊天框：确认两行文案随 agent 状态实时变化、毛玻璃与圆角在深/浅背景上可见、
   文字在两种背景下可读；
   点击右下角箭头收起/展开并写回 `chat_open`，宠物屏幕位置不变；拖动宠物时气泡跟随；
   点击气泡不会拖动窗口；收起后原气泡区域点击穿透。
8. Herdr 前台时气泡和按钮同时消失，切到其它应用后恢复；Herdr 在后台时点宠物或
   气泡会拉起 Herdr 并跳到对应终端；拖动宠物不会拉起 Herdr（前三项已实机验证，
   冷启动未验证）。
9. 无 blocked/working/error 且 done 都已读时气泡和按钮同时消失，只留宠物；
   agent 进入 error 时气泡保留。done 未读时气泡保留，把 attention.seen 改为 true
   后 500 ms 内消失（已用假 herdr + 假 view.json 验证）。
10. 多 agent 时确认气泡标题按 blocked/error → 未读 done → 最近状态变化的 working
    → 其它切换；同组按状态时间先后。单测覆盖，多 agent 实机待回归。

测试素材：测试内用 `image` 生成 1536×1872 的合成 atlas；不提交任何用户宠物素材。

## 6. 里程碑

| 阶段 | 内容 | 验证 |
| --- | --- | --- |
| M0 | 文档、Cargo 脚手架、工具链 | `cargo build` 通过 |
| M1 | `pet.rs` + `status.rs` + 单测 | `cargo test` |
| M2 | `herdr.rs` + 假服务端测试 | `cargo test` |
| M3 | 窗口 + 渲染 + 动画 | 截图、肉眼确认 |
| M4 | 拖拽、凝视、点击穿透、托盘 | 手工交互 |
| M5 | 提醒、音效、配置持久化 | 触发真实状态 |
| M6 | 手工回归、CLAUDE.md 与文档校准 | 全量检查 |

## 7. 风险与待定项

- **wgpu 30 API 变动**：已按 wgpu 30 实际 API 校准；渲染层保持薄。
- **macOS 透明窗口合成**：wgpu 30 + Metal 下必须用 `PostMultiplied` 并把
  片元颜色预乘 alpha；`PreMultiplied` 会出现黑底、直通 alpha 会在浅色背景
  出现白边（均已实机验证，见 3.4）。
- **tray-icon × winit**：已验证 0.26 + winit 0.30 在 `resumed` 中创建、
  菜单事件非阻塞轮询可用；保留 3.7 的降级路径。
- **AppKit 浮层 × wgpu**：`NSVisualEffectView` 挂在 winit 内容视图的子视图上，
  位于 CAMetalLayer 之上；容器 `hitTest:` 必须返回 nil，否则按钮点击会被 AppKit
  吞掉。已在 macOS 实机验证模糊、圆角、Retina point 坐标、按钮切换与拖动。
- **Herdr.app 前台判断**：主线程每 250 ms 查询一次 NSWorkspace，不启动子进程；
  Herdr 前台时整个聊天 UI 隐藏，不区分当前 tab。点击跳转使用官方
  `herdr-app do agent.focus`，不读写其内部 socket；`agent.focus` 对隐藏终端/子 agent
  的行为以 CLI 文档为准；冷启动超过 5 秒时跳转可能失败并记录日志。绝不停止或重启
  用户的 Herdr/core。
- **Herdr 已读状态**：done 的 seen 读取 Herdr 内部 `view.json`（无公开 CLI），
  每 500 ms 读一次、约 20 KB；解析失败保留上次结果，首次失败按全部未读。Herdr
  未运行或文件不可读时 done 气泡会一直保留；这是「未查看不消失」的保守行为。
  远端终端的 attention 键可能是 `machine/tN`，当前按终端地址精确匹配，未命中时
  按未读处理。
- **焦点抢占**：Accessory 策略下点击宠物目前不会把 App 带到前台；若后续
  改为 NSPanel/nonactivating 或行为变化，再按 3.5 处理。
- **多显示器坐标**：首版用主屏的 scale/高度换算 `NSEvent` 全局坐标；
  窗口在副屏或混合缩放组合下可能偏差，后续应改用 NSScreen 逐屏换算。
- **素材授权**：只读取用户自己安装的宠物；仓库与发布包不内置官方宠物。

## 8. 参考

- OpenAI 官方宠物文档：[Pets](https://learn.chatgpt.com/docs/pets)（状态含义与素材
  格式；未公开随机待机规则）；开源终端实现 [`openai/codex` ambient.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/pets/ambient.rs)
  （idle/running/waiting/review/failed 循环及一次性动作 fallback）。
- Codex 宠物契约与动作规范：
  `~/.codex/skills/hatch-pet/references/codex-pet-contract.md`、
  `animation-rows.md`（本机安装；若不存在，按本文件第 3.1 节为准）。
- 本机宠物样例：`~/.codex/pets/{diana,frieren,endminguga}/`。
- herdr-core 协议：`hc schema`、`hc events --help`、`hc connect --help`。
