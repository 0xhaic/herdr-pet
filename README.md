# herdr-pet

macOS 桌面宠物：宠物动画由本机 herdr-core 中所有 agent 的实时状态驱动。
宠物素材沿用 Codex 格式，程序本身不内置任何宠物。

## 特性

- **独立透明窗口**：无边框、始终置顶、不显示 Dock 图标；切换 Space 或进入其它应用的全屏空间时保持可见。
- **逐像素点击穿透**：鼠标落在宠物透明区域时穿透到桌面，落在宠物本体上可拖拽移动；松开后自动保存位置。拖动只约束宠物本体（气泡和按钮可超出屏幕），按宠物当前所在屏幕取值、可直接拖到其它屏幕，上边界不进入菜单栏。
- **状态汇总**：汇总本机所有 herdr agent，一只宠物显示最紧急的状态，优先级
  `Blocked > Error > Working > Done > Idle`（Working 高于 Done，站立只表示有任务在执行）。
- **聊天框**：宠物上方固定两行的毛玻璃气泡，显示最紧急 agent 的任务标题与最新动作
  （`status.msg`；缺省回退中文状态，working 缺省显示省略号循环的 `Thinking...`）。
  标题行文字前显示 agent 图标，取自 Herdr.app
  的 program 图标目录（`HERDR_APP_HOME/icons`）：已知 agent 使用对应品牌/单色图标，
  未知 agent 使用通用 agent 图标；Herdr.app 或图标文件不存在时自动不显示并恢复文字
  宽度。第二行文字前显示状态图标（空闲 `moon.zzz`、等待输入 `hand.raised`、已完成
  `checkmark.circle`、出错 `xmark.circle`，无 agent 时 `ellipsis.circle`），使用系统
  语义色（灰/橙/绿/红）；工作中显示系统旋转加载指示器，仅在气泡可见时运行动画。
  宠物右下角的箭头按钮收起/展开，开关写入配置；气泡与箭头按钮共用同一套玻璃
  样式和 `chat_opacity` 不透明度，默认 `0.7`；文字与箭头颜色随系统浅色/深色外观
  切换，避免亮色背景上的白字不可读。
- **气泡内容优先级**：`blocked`/`error` → 未读 `done` → 最近状态变化的 `working`
  → 其它。同一档取时间最早（blocked/error、未读 done）；`working` 取最近一次状态
  变化作为「最后一次交互」的近似（herdr 没有逐标签页交互时间）。同一档内，子
  agent（`parent` 非空）排在其父 agent 前，因此 herdr 派发子 agent 后气泡与点击
  都指向子 agent；父 agent 若处于更高档（如 blocked/error）仍优先。宠物动画仍按
  `Blocked > Error > Working > Done > Idle` 的聚合优先级（Done 再按 Herdr 已读
  状态分为 review 站立与坐姿），因此气泡和动画可能指向不同 agent。
- **会话切换**：有多个会话需要气泡（`blocked` / `working` / `error` / 未读 `done`）
  时，气泡右端显示左右箭头，点击按上述优先级在候选会话间循环切换。手动选择后气泡
  保持该会话，状态变化照常刷新它的两行文案；当它不再需要气泡（被处理、被 Herdr
  查看或状态清除）时，自动回落到剩余候选中的最高优先级会话。
- **显示时机**：通常只有 Herdr 不在前台（或未启动）、且存在需要关注的 agent 时才显示
  聊天 UI：`blocked` / `working` / `error` 始终算；`done` 在 Herdr 尚未标记已读时
  也算，查看后自动消失；只有已读 `done` / `idle` 时整体隐藏。例外：任一 agent 处于
  `blocked` / `error`，或存在未读 `done` 时，气泡无视 Herdr 是否前台、是否已收起而
  强制弹出，直到 agent 被手动处理或该 done 被 Herdr 查看；此时若 Herdr 在前台则不
  显示箭头按钮。宠物本体始终保留。
- **与 Herdr 跳转**：气泡可见时，左键点击气泡或宠物会拉起 Herdr 并跳到对应
  会话；子 agent（`parent` 非空）没有独立 pane，改用 `terminal.peek` 在其父会话
  的 tab 上打开浮层窗口，peek 不可用时回退到父会话所在的 tab。拖动宠物不会触发
  跳转。
  Herdr 在前台时聊天 UI 默认隐藏，点击宠物仍可跳转。
- **状态动画**：working 奔跑、blocked 挥手等待、done 跳跃一次后进入 review 站立
  （Herdr 未标记已读时循环，查看后回到坐姿）、error 播放失败动作；
  连续 idle 15–30 秒会随机做一次挥手/跳跃小动作（本项目增强）。
- **鼠标悬停**：仅当宠物正在播放 idle 动画（Row 0）时，悬停才播放 3 次跳跃后坐姿
  待机；working、blocked、review、done 入场、failed、拖拽或凝视期间悬停无反应。
  移开后恢复当前状态。
- **鼠标凝视**：空闲时、以及 done 连续保持 30–90 秒的凝视窗口内（未悬停）宠物看向
  鼠标方向（16 个方向，需素材包含 look 行）；done 窗口结束后回到 idle 坐姿。
  done 前 30 秒与窗口结束后不响应凝视；未查看的 done 在 review 站立期间不凝视，
  查看后若仍在窗口内，坐姿可立即凝视。
- **提醒**：agent 进入 blocked / done 时弹系统通知并播放提示音（Basso / Glass）；
  是否提醒与是否播放音效可分别开关，同一帧内的多条提醒会合并。
- **菜单栏控制**：显示/隐藏、设置、关于、退出；其它设置项统一放在设置面板中。
- **设置面板**：原生小窗口，可切换宠物、选择宠物目录、调整大小与气泡不透明度、开关
  提醒/音效/凝视/点击穿透/聊天气泡；改动立即生效，停止操作后写回配置。点击面板外
  任意位置自动关闭（与「关于」一致）。

## 前置条件

- macOS（首版仅支持 macOS）。
- Rust stable 与 Cargo（本项目按 1.100 验证）。rustup 安装后若 `cargo` 不在 PATH，先执行
  `source "$HOME/.cargo/env"`。
- 本机正在运行 herdr-core。程序通过 Unix socket 订阅事件，发现顺序为
  `HC_TERMINAL_SOCKET` → `$XDG_STATE_HOME/herdr-core/<session>/<session>.sock`
  （session 取 `HC_SESSION`，默认 `default`）；连接断开时后台自动重连，宠物保留最后状态。
- 至少一只 Codex 宠物，位于 `$CODEX_HOME/pets`（默认 `~/.codex/pets/<name>/`），
  目录内包含 `pet.json` 与 `spritesheet.webp`；可在设置面板中改用其它宠物目录。
- Herdr.app（可选）：前台判断使用 `NSWorkspace`，点击跳转通过
  `herdr-app do agent.focus`（普通 agent）或 `terminal.peek`（子 agent）实现；
  气泡标题行的 agent 图标也读取 Herdr.app 的
  program 图标目录。未安装或找不到时宠物其余功能正常，图标隐藏，点击只会尝试
  打开 Herdr。

## 构建与运行

```bash
source "$HOME/.cargo/env"
cargo build --release
cargo run --release            # 启动宠物（需在已登录的 macOS GUI 会话中）
```

选择宠物：

```bash
cargo run -- --list-pets                 # 列出可加载宠物及显示名
cargo run -- --pet diana                 # 按宠物目录下的子目录名选择（默认 ~/.codex/pets）
cargo run -- --pet /path/to/pet-dir      # 或直接指定宠物目录
```

不指定 `--pet` 时使用配置里的 `pet`，否则取搜索目录中第一只可加载宠物。
`--pet` 与 `--list-pets` 不能同时使用。搜索目录默认是 `$CODEX_HOME/pets`，
可用配置项 `pets_dir` 或设置面板中的「宠物目录」更改。

## 打包为 .app

本地打包（产物为 `dist/herdr-pet.app`）：

```bash
scripts/bundle.sh                      # 本机架构
scripts/bundle.sh aarch64-apple-darwin # 指定目标
```

脚本在 `dist/` 下组装 `Contents/MacOS/herdr-pet`、`Contents/Resources/AppIcon.icns`
与 `packaging/Info.plist`，并做 ad-hoc 签名（不含 Developer ID 与公证）。

`.app` 内使用应用自身身份发送系统通知，通知横幅的图标与应用图标一致；首次发送时
macOS 会请求一次通知权限，允许后生效。`cargo run` 直接运行裸二进制时没有 bundle
身份，会回落到 `osascript`，此时图标由系统按 Script Editor 显示。

应用图标采用「探头」方向（`assets/app-icon.svg` → `packaging/AppIcon.icns`，1024 px，
824 px 圆角底板）。应用以 `LSUIElement` 运行，不在 Dock 显示；图标用于 Finder、DMG 与通知。
设计探索稿保留在本地 `design/logo-preview.html`（不随仓库分发，不参与打包）。

推送到 GitHub 后，`.github/workflows/macos-release.yml` 可在 CI 中打包：

- 推送 `v*` tag（如 `v0.1.17`）自动触发，或从 Actions 页面手动触发；
- tag 构建先校验 tag 名与 `Cargo.toml`、`Cargo.lock` 中的版本一致，不一致直接失败；
- 发布前必须在 `docs/release-notes/<tag>.md` 编写版本说明（如
  `docs/release-notes/v0.1.17.md`）；tag 构建校验文件存在，旧 tag 缺失时从默认
  分支读取，仍缺失直接失败；
- 先运行 `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`；
- 交叉编译 arm64，产出 `herdr-pet-<version>-macos-arm64.dmg` 并上传为 artifact；
- tag 构建用该说明文件创建或更新同名 GitHub Release，并附带该 dmg
  （手动触发只产出 artifact）。

下载的 dmg 未签名/未公证，首次启动需右键 → 打开，
或执行 `xattr -dr com.apple.quarantine /Applications/herdr-pet.app`。

## 菜单栏

| 菜单项 | 说明 |
| --- | --- |
| 显示/隐藏 | 切换窗口可见性 |
| 设置… | 打开设置面板，实时修改宠物与各项配置 |
| 关于 | 显示版本号与仓库链接，可打开 GitHub 仓库 |
| 退出 | 保存配置并退出 |

托盘图标是与应用图标同轮廓的方形模板图（`assets/tray-icon.png`，22 pt @2x，源文件
`assets/tray-icon.svg`）：圆角底板实心、猫头镂空；作为 macOS 模板图随菜单栏明暗自动
反色；切换宠物不改变它。
所有设置项都在设置面板中；面板中的修改会写回配置文件。

## 配置

路径：`$XDG_CONFIG_HOME/herdr-pet/config.json`，未设置时为
`~/.config/herdr-pet/config.json`。写入采用临时文件 + rename 原子替换；
文件缺失或损坏时回落到默认值，不会覆盖用户文件，直到下一次显式保存。

```json
{
  "pets_dir": null,
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

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `pets_dir` | `null` | 宠物根目录，每个子目录一只宠物；空则使用 `$CODEX_HOME/pets`（默认 `~/.codex/pets`）。可在设置面板中更换或恢复默认 |
| `pet` | `null` | 启动时使用的宠物名或目录；空则自动选第一只可加载宠物 |
| `size` | `104` | 宠物高度（logical px），限制在 52–312 |
| `position` | `null` | 宠物左上角（物理像素，左上角原点）；收起/展开聊天框不会移动宠物。启动时若窗口在所有屏幕之外则回落到默认位置 |
| `notifications` | `true` | 是否发送系统通知 |
| `notification_sound` | `true` | 是否播放提醒音效 |
| `gaze` | `true` | 凝视开关（idle 与 done 凝视窗口；素材无 look 行时自动禁用） |
| `click_through` | `false` | 整窗点击穿透；开启后宠物不可拖拽，只能从设置面板关闭 |
| `chat_open` | `true` | 聊天框是否展开；宠物右下角箭头按钮切换并写回。仅在聊天 UI 可见时生效 |
| `chat_opacity` | `0.7` | 气泡与箭头按钮玻璃层的不透明度（0–1，越大越不透明）；`0` 时只显示文字与箭头，可在设置面板中实时调整 |

## 状态与动画

| 宠物状态 | 动画 |
| --- | --- |
| working | running（循环） |
| blocked | waving 一次 → waiting（循环） |
| done（未查看） | jumping 一次 → review（循环站立）；Herdr 标记已读后直接回坐姿，不重播 jumping |
| done（已查看） | jumping 一次 → 坐姿；连续 done 30 秒后开放 60 秒凝视窗口，窗口结束回 idle 坐姿 |
| error | failed（循环） |
| idle | idle（循环）；连续 idle 15–30 秒随机 waving / jumping 一次后回到 idle |
| 拖动中 | running-left / running-right |
| 鼠标悬停（仅 Row 0） | jumping ×3 → idle；其它动画行/状态期间不触发 |
| idle / done 30–90 秒 + 鼠标 | look 行 9/10，按 16 方向选列 |

只统计有 agent 状态的本机终端；普通 zsh/bash 等无状态终端按 idle 处理。
herdr-core 清除终端状态时（例如 agent 退出、root shell 回到前台），宠物立即回到 idle。

提醒只在状态**变化沿**触发：进入 blocked 或 done 时提醒，初始快照不提醒；
拖拽、凝视会打断 idle 计时与随机动作；悬停仅在 idle 行触发，只打断 idle 计时。

## 宠物素材

宠物文件视为不可信输入；运行中切换失败会跳过该宠物并保留当前宠物，不崩溃：

- `pet.json`：`id`、`displayName`、`description`、`spriteVersionNumber`（默认 1）、
  `spritesheetPath`（默认 `spritesheet.webp`，必须是宠物目录内的相对路径）。
- 精灵图：宽固定 `1536`，高为 `208` 的整数倍且至少 9 行（1872）；
  凝视行 9–10 需要至少 11 行（2288），不足时禁用凝视；文件不超过 20 MiB。
- 仓库不包含、也不分发 Codex 官方宠物或任何用户 `~/.codex/pets` 素材，
  请自行安装 Codex 宠物后使用。

## 限制与已知问题

- 仅支持 macOS（CI 仅产出 arm64 构建）；从源码运行，或使用 `scripts/bundle.sh`
  自行打包，或下载 GitHub Actions 产出的未签名/未公证 `.app`。
- 多显示器、混合缩放下全局坐标按主屏换算，宠物放在副屏时位置/凝视可能有偏差。
- 前台判断每 250 ms 查询一次 `NSWorkspace`；Herdr 在前台时聊天 UI 整体隐藏，切走后
  恢复。Herdr 启动中的点击跳转依赖 5 秒内完成启动，极慢的冷启动可能需要再点一次。
- Herdr 在前台时无法通过箭头展开/收起气泡（按当前显示规则，整个聊天 UI 不可见）。
- `done` 的已读状态读取 Herdr 的 `view.json`（每 500 ms，约 20 KB）：`attention`
  中 `seen` 为 false 或没有记录时按未读处理，气泡保留；Herdr 打开并查看该终端后
  标记 seen，下一次轮询即消失，宠物同时从 review 站立回到坐姿。Herdr 未运行/该文件
  不可读时 done 会一直按未读处理；此时没有查看入口，气泡会保留、宠物会保持 review
  站立。该文件是 Herdr 的内部状态，非公开 CLI。
- 「最后一次交互的 working」用该终端最近一次状态变化时间近似；herdr-core 不提供
  用户对标签页的交互时间戳，切换标签但状态未变化时不会更新这个顺序。
- 不做点击养成/亲密度、herdr 终端内浮层、宠物生成。

## 开发

```bash
cargo test                                  # 单元测试
cargo build
cargo fmt --check
cargo clippy --all-targets -- -D warnings   # 交付前必须全绿
```

协议、状态优先级、动画时长与里程碑以 [`docs/design.md`](docs/design.md) 为准；
模块划分与实现约束见 [`CLAUDE.md`](CLAUDE.md)。

## 开源协议

[MIT](LICENSE) © 2026 0xhaic
