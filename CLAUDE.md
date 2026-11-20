# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目是什么

`herdr-pet` 是一个 Rust 桌面宠物（macOS 优先）：复刻 Codex 桌面版的宠物表现，
宠物动画由 herdr-core 中所有 agent 的状态驱动。宠物素材沿用 Codex 格式
（`~/.codex/pets/<name>/pet.json` + `spritesheet.webp`），程序不自带素材。

完整方案、协议细节、动画时长表、里程碑见 **`docs/design.md`**；动手前先读它。
M0–M6 已实现：纯逻辑（pet/status/config）、herdr 客户端、wgpu 渲染、
macOS 交互（拖拽/凝视/点击穿透）、托盘与提醒。改动后按下文验证门槛回归。

## 已确定的决策（不要重新讨论）

- 形态：独立桌面窗口（不是 herdr 终端 pane 内浮层）。
- 状态：汇总全部 agent，一只宠物显示最紧急状态。
- 技术栈：`winit` + `wgpu`（主版本已在 `Cargo.toml` 声明；caret 版本允许补丁升级）。
- 范围：基础交互（拖拽/位置记忆/托盘）、鼠标凝视（v2 look rows）、blocked/done 提醒与音效。
- 不做：点击养成/亲密度、herdr 内浮层、宠物生成、跨平台适配（首版）。

## 常用命令

Rust 由 rustup 安装在 `~/.cargo/bin`，安装时未改 PATH；每个 shell 先执行：

```bash
source "$HOME/.cargo/env"
```

```bash
cargo build                     # 编译
cargo run                       # 启动桌面宠物（需在已登录的 GUI 会话中）
cargo test                      # 全部单元测试
cargo test pet::                # 只跑某个模块的测试（也可用测试名过滤）
cargo test <test_name> -- --nocapture
cargo fmt --check               # 格式检查
cargo clippy --all-targets -- -D warnings   # 静态检查，交付前必须通过
```

CLI：`cargo run -- --list-pets`、`cargo run -- --pet <name|path>`。

## 版本与发布

- 版本号唯一来源是 `Cargo.toml` 的 `version`，遵循语义化版本；发布 tag 名固定为
  `v<version>`（如 `v0.1.0`），必须与 `Cargo.toml` 一致。
- 发布提交必须在 `main` 上，且先满足「验证门槛」；发布前更新版本号、README 与
  `docs/design.md` 中的状态说明。
- 推送 `v<version>` tag 会触发 `.github/workflows/macos-release.yml`：构建
  `herdr-pet-<version>-macos-arm64.dmg`、上传 artifact，并创建同名 GitHub Release。
- 打 tag 前需明确确认；已推送的 tag 不删除、不移动，也不重写已发布的历史，
  修复问题后发布新版本。
- 发布产物是 ad-hoc 签名、未公证的 arm64 `.app`；Release 说明必须保留首次启动的
  Gatekeeper 提示。

## 模块布局

```text
src/
  main.rs        winit 事件循环、60 Hz tick、模块编排
  herdr.rs       herdr-core JSON-RPC 客户端（后台 tokio 线程）
  status.rs      终端状态表、聚合、blocked/done 提醒判定（纯逻辑）
  pet.rs         pet.json/atlas 加载、动作行、动画状态机、凝视（纯逻辑）
  render.rs      wgpu 单纹理 quad、透明合成
  platform/     macOS 窗口交互：全局鼠标、拖拽、点击穿透、托盘
  config.rs      ~/.config/herdr-pet/config.json 读写
  notify.rs      afplay + 系统通知（.app 内原生发送，裸二进制回落 osascript）
  tray.rs        tray-icon 菜单
```

## 必须遵守的约束

- **先读 `docs/design.md`**；状态优先级、动画映射、帧时长、JSON-RPC 事件类型以它为准，
  不要凭感觉改。关键点：
  - 聚合优先级 `Blocked > Error > Working > Done > Idle`；只统计 live 且带 status 的终端。
    Working 高于 Done：只有任务执行中才显示站立（running），done 只做一次跳跃提醒。
  - 计数按 terminal address；父 agent 与子 agent（`parent` 非空）各算一个，不做父子去重。
  - 动画：working→running，blocked→waving→waiting，done→jumping→idle，
    error→failed，idle→idle，拖动→running-left/right；鼠标悬停覆盖除拖拽外的
    状态，播放 jumping ×3 后回到 idle；凝视在 idle 及 done 跳跃结束后的坐姿、
    未悬停时覆盖；离开凝视回到坐姿，不重播 jumping。
    连续 idle 满 15–30 秒随机播放一次 waving/jumping 后回 idle（本项目增强，
    非 Codex 公开契约）；凝视/拖拽/悬停打断并重置 idle 计时。
  - 初始 snapshot 不提醒；只在进入 blocked/done 的变化沿提醒，同帧合并。
- **素材授权**：不提交、不内置 Codex 官方宠物或任何用户 `~/.codex/pets` 素材；
  测试用合成 atlas，不依赖本机宠物。
- **herdr-core 用事件订阅，不轮询**：`hello` → `events.subscribe`，处理
  `created/status/finished/lost/removed/title/labels/missed`；`missed` 或断线直接重连。
  Socket 发现顺序：`HC_TERMINAL_SOCKET` → `~/.local/state/herdr-core/<session>/<session>.sock`
  （`HC_SESSION`，默认 `default`）。
- **主线程不得阻塞**：网络、重连、通知子进程都在后台；主线程每帧非阻塞 drain 通道。
- **纯逻辑与平台代码分离**：`pet.rs`、`status.rs`、配置与协议解析必须可在无 GUI/无 herdr
  环境下单测；unsafe 只允许出现在 macOS 平台胶水中，并注明原因。
- **宠物文件视为不可信输入**：spritesheet ≤ 20 MiB；宽 1536、高为 208 的整数倍且 ≥ 9 行；
  `spritesheetPath` 拒绝绝对路径与目录穿越。失败时保留当前宠物，不崩溃。
- **通知安全**：`.app` 内用原生通知（图标与 `AppIcon` 统一）；osascript 回落路径中
  外部文本（agent 标题等）必须做 AppleScript 字符串转义，命令参数用数组传递，
  禁止拼接 shell 字符串；不得把用户数据发到外部服务。
- **透明合成固定为 `PostMultiplied` + 预乘 alpha 输出**：wgpu 30 的 Metal
  后端下 `PreMultiplied` 会产生黑底，直通 alpha 会在浅色背景产生白边
  （见 design 3.4），不要改回。
- **配置原子写**：临时文件 + rename；路径固定为
  `$XDG_CONFIG_HOME/herdr-pet/config.json`，未设置时 `~/.config/herdr-pet/config.json`
  （不要用 `dirs::config_dir()`，macOS 上会指向 Application Support）。
  损坏/缺失时回落到默认值并记录，不覆盖用户文件直到下一次显式保存。
- **依赖克制**：不新增 crate（尤其第二套 GUI/运行时）除非先征得同意；
  能用现有依赖 + std 解决就不要再引。
- **验证门槛**：`cargo test`、`cargo build`、`cargo clippy --all-targets -- -D warnings`
  全绿；GUI 改动需实际启动并截图确认透明/置顶/拖拽/凝视。
- **推送远程仓库**：远程仓库使用 `0xhaic/herdr-pet`。未获明确要求不得 commit/push；
  推送前必须满足上文「验证门槛」。`v*` tag 会触发 CI 打包并创建 GitHub Release，
  打 tag 前先确认。

## 环境与参考

- macOS（arm64），Rust 1.100 stable，rustfmt/clippy 已装；Xcode/clang 可用。
- herdr-core 0.1.21 本机在运行；`hc schema` 可查完整 JSON-RPC 契约，
  `hc events --help`、`hc connect --help` 可查行为。
- Codex 宠物规范（本机若存在）：
  `~/.codex/skills/hatch-pet/references/codex-pet-contract.md`、
  `~/.codex/skills/hatch-pet/references/animation-rows.md`；
  样例宠物在 `~/.codex/pets/`。若这些文件不存在，以 `docs/design.md` 第 3.1 节为准。
- `docs/design.md` 是方案的唯一事实来源；实现与文档冲突时先改文档、再改代码。
