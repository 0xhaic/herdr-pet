# herdr-pet

macOS 桌面宠物：宠物动画由本机 herdr-core 中所有 agent 的实时状态驱动。
宠物素材沿用 Codex 格式，程序本身不内置任何宠物。

## 特性

- **独立透明窗口**：无边框、始终置顶、不显示 Dock 图标；切换 Space 或进入其它应用的全屏空间时保持可见。
- **逐像素点击穿透**：鼠标落在宠物透明区域时穿透到桌面，落在宠物本体上可拖拽移动；松开后自动保存位置。
- **状态汇总**：汇总本机所有 herdr agent，一只宠物显示最紧急的状态，优先级
  `Blocked > Error > Working > Done > Idle`（Working 高于 Done，站立只表示有任务在执行）。
- **状态动画**：working 奔跑、blocked 挥手等待、done 跳跃一次后回到坐姿、error 播放失败动作；
  连续 idle 15–30 秒会随机做一次挥手/跳跃小动作（本项目增强）。
- **鼠标悬停**：悬停时播放 3 次跳跃后坐姿待机，移开后恢复当前状态（与 Codex 一致）。
- **鼠标凝视**：空闲时、以及 done 连续保持 30–90 秒的凝视窗口内（未悬停）宠物看向
  鼠标方向（16 个方向，需素材包含 look 行）；done 窗口结束后回到 idle 坐姿。
  done 前 30 秒与窗口结束后不响应凝视。
- **提醒**：agent 进入 blocked / done 时弹系统通知并播放提示音（Basso / Glass）；
  是否提醒与是否播放音效可分别开关，同一帧内的多条提醒会合并。
- **菜单栏控制**：显示/隐藏、切换宠物、缩放、点击穿透、提醒开关、通知音效、退出。

## 前置条件

- macOS（首版仅支持 macOS）。
- Rust stable 与 Cargo（本项目按 1.100 验证）。rustup 安装后若 `cargo` 不在 PATH，先执行
  `source "$HOME/.cargo/env"`。
- 本机正在运行 herdr-core。程序通过 Unix socket 订阅事件，发现顺序为
  `HC_TERMINAL_SOCKET` → `$XDG_STATE_HOME/herdr-core/<session>/<session>.sock`
  （session 取 `HC_SESSION`，默认 `default`）；连接断开时后台自动重连，宠物保留最后状态。
- 至少一只 Codex 宠物，位于 `$CODEX_HOME/pets`（默认 `~/.codex/pets/<name>/`），
  目录内包含 `pet.json` 与 `spritesheet.webp`。

## 构建与运行

```bash
source "$HOME/.cargo/env"
cargo build --release
cargo run --release            # 启动宠物（需在已登录的 macOS GUI 会话中）
```

选择宠物：

```bash
cargo run -- --list-pets                 # 列出可加载宠物及显示名
cargo run -- --pet diana                 # 按 ~/.codex/pets 下的目录名选择
cargo run -- --pet /path/to/pet-dir      # 或直接指定宠物目录
```

不指定 `--pet` 时使用配置里的 `pet`，否则取搜索目录中第一只可加载宠物。
`--pet` 与 `--list-pets` 不能同时使用。

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

- 推送 `v*` tag（如 `v0.1.7`）自动触发，或从 Actions 页面手动触发；
- 先运行 `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`；
- 交叉编译 arm64，产出 `herdr-pet-<version>-macos-arm64.dmg` 并上传为 artifact；
- tag 构建会同时创建同名 GitHub Release 并附带该 dmg（手动触发只产出 artifact）。

下载的 dmg 未签名/未公证，首次启动需右键 → 打开，
或执行 `xattr -dr com.apple.quarantine /Applications/herdr-pet.app`。

## 菜单栏

| 菜单项 | 说明 |
| --- | --- |
| 显示/隐藏 | 切换窗口可见性 |
| 下一只宠物 | 切换到 `~/.codex/pets` 中的下一只宠物 |
| 放大 / 缩小 | 按 1.25 倍调整大小，高度限制在 52–312 logical px |
| 点击穿透 | 整窗鼠标穿透；开启后宠物完全不接收鼠标、不能拖拽，只能从托盘关闭 |
| 提醒 | 系统通知与提醒音效的总开关 |
| 通知音效 | 只关闭提醒音效，系统通知照常发送 |
| 关于 | 显示版本号与仓库链接，可打开 GitHub 仓库 |
| 退出 | 保存配置并退出 |

托盘图标是与应用图标同轮廓的方形模板图（`assets/tray-icon.png`，22 pt @2x，源文件
`assets/tray-icon.svg`）：圆角底板实心、猫头镂空；作为 macOS 模板图随菜单栏明暗自动
反色；切换宠物不改变它。
菜单中的开关、切换宠物与尺寸变化会写回配置文件。

## 配置

路径：`$XDG_CONFIG_HOME/herdr-pet/config.json`，未设置时为
`~/.config/herdr-pet/config.json`。写入采用临时文件 + rename 原子替换；
文件缺失或损坏时回落到默认值，不会覆盖用户文件，直到下一次显式保存。

```json
{
  "pet": "diana",
  "size": 104,
  "position": [1512, 812],
  "notifications": true,
  "notification_sound": true,
  "gaze": true,
  "click_through": false
}
```

| 字段 | 默认 | 说明 |
| --- | --- | --- |
| `pet` | `null` | 启动时使用的宠物名或目录；空则自动选第一只可加载宠物 |
| `size` | `104` | 宠物高度（logical px），限制在 52–312 |
| `position` | `null` | 窗口位置（物理像素，左上角原点）；启动时若在所有屏幕之外则回落到默认位置 |
| `notifications` | `true` | 是否发送系统通知 |
| `notification_sound` | `true` | 是否播放提醒音效 |
| `gaze` | `true` | 凝视开关（idle 与 done 凝视窗口；素材无 look 行时自动禁用） |
| `click_through` | `false` | 整窗点击穿透；开启后宠物不可拖拽 |

## 状态与动画

| 宠物状态 | 动画 |
| --- | --- |
| working | running（循环） |
| blocked | waving 一次 → waiting（循环） |
| done | jumping 一次 → 坐姿；连续 done 30 秒后开放 60 秒凝视窗口，窗口结束回 idle 坐姿 |
| error | failed（循环） |
| idle | idle（循环）；连续 idle 15–30 秒随机 waving / jumping 一次后回到 idle |
| 拖动中 | running-left / running-right |
| 鼠标悬停 | jumping ×3 → idle；覆盖除拖拽外的状态 |
| idle / done 30–90 秒 + 鼠标 | look 行 9/10，按 16 方向选列 |

提醒只在状态**变化沿**触发：进入 blocked 或 done 时提醒，初始快照不提醒；
拖拽、凝视或悬停会打断 idle 计时与随机动作。

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
