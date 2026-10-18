# Faz 功能需求清单

本文档列出 Faz 当前**已实现**的功能需求，每条都可追溯到代码位置。它不是路线图，也不是产品设想——写进来的功能都在主分支上可用，且标注了验证方式。

版本基准：`config_version 34`，五个顶层菜单（Faz / Shell / Edit / View / Window）。

---

## 1. 终端核心

### 1.1 PTY 与会话

| 编号 | 需求 | 状态 | 验证 |
|---|---|---|---|
| T-101 | 每个 pane 拥有独立 PTY，本地 shell 运行在 slave 上 | 已实现 | `mux/src/domain.rs` |
| T-102 | pane 输出经 socketpair 交给解析线程，与 PTY 读取解耦 | 已实现 | `mux/src/lib.rs` `read_from_pane_pty` |
| T-103 | PTY 读错误中 `EAGAIN`/`EINTR`/`TimedOut` 视为可重试，不作致命处理 | 已实现 | `is_retryable_read`，单测覆盖 |
| T-104 | PTY 真正失效（`EBADF`/`EIO`）时显示提示并强制移除 pane | 已实现 | `mux/src/lib.rs` `pty_fatal` |

### 1.2 终端语义

| 编号 | 需求 | 状态 | 验证 |
|---|---|---|---|
| T-201 | VT 序列解析、屏幕缓冲、滚动历史 | 已实现 | `term/` |
| T-202 | 同步输出（Synchronized Output）原子渲染 | 已实现 | `SynchronizedOutputState`，单测覆盖 |
| T-203 | 选区、复制、粘贴、鼠标追踪 | 已实现 | `term/`、`faz-gui/src/termwindow/` |
| T-204 | 超链接识别：URL、路径、裸域名 | 已实现 | `docs/features.md` |
| T-205 | Option+Click 在当前输入行内移动光标 | 已实现 | `docs/features.md` |
| T-206 | 中日韩宽字符与组合字符渲染 | 已实现 | `termwiz/`、字体 shaper |

---

## 2. 窗口与布局

| 编号 | 需求 | 状态 | 验证 |
|---|---|---|---|
| W-101 | 多 tab、多 pane、水平/垂直分屏 | 已实现 | `mux/src/tab.rs` |
| W-102 | pane 缩放（zoom）、pane 导航与选择器 | 已实现 | `TogglePaneZoomState`、`paneselect.rs` |
| W-103 | 多窗口，窗口位置尺寸持久化 | 已实现 | `window/src/os/macos/window.rs` |
| W-104 | 窗口快照：关闭/隐藏时保存布局，可从 Shell 菜单恢复 | 已实现 | `docs/features.md` |
| W-105 | 全屏、居中、填满、置顶/置底 | 已实现 | View 菜单 |
| W-106 | 标签页重命名 | 已实现 | `tab_rename.rs` |
| W-107 | 标签栏位置与样式可配置 | 已实现 | `config/` |

---

## 3. 配置系统

| 编号 | 需求 | 状态 | 验证 |
|---|---|---|---|
| C-101 | Lua 配置，支持热重载 | 已实现 | `config/src/lib.rs` |
| C-102 | 配置版本号与迁移：`config_version` 随发布递增 | 已实现 | `assets/shell-integration/config_version.txt`、`scripts/check_release_config.sh` |
| C-103 | 版本历史与迁移规则记录在文档 | 已实现 | `docs/config-versions.md` |
| C-104 | 配置 TUI（`faz config`）进行交互式设置 | 已实现 | `faz-cli/src/config_cmd.rs` |
| C-105 | 自定义键绑定，**必须追加**而非替换 `config.keys` | 已实现 | `docs/configuration.md` |
| C-106 | 主题与配色 | 已实现 | `config/`、`docs/configuration.md` |
| C-107 | 系统代理检测；内网/回环地址直连 | 已实现 | `config/src/proxy.rs` |

**约束**：不引入 `config.language` 式的 UI 多语言。历史上 `rust-i18n` 方案已回退，UI 字符串保持英文。

---

## 4. Shell 集成

| 编号 | 需求 | 状态 | 验证 |
|---|---|---|---|
| S-101 | zsh / bash / fish 集成脚本安装（`faz init`） | 已实现 | `assets/shell-integration/` |
| S-102 | 通过 user-var 向终端报告 cwd、git 分支、命令状态 | 已实现 | `assets/shell-integration/` |
| S-103 | shell 集成可重置（`faz reset`） | 已实现 | `faz-cli/src/reset.rs` |
| S-104 | 生成的脚本通过 shellcheck 与 `zsh -n` 语法检查 | 已实现 | CI `checks.yml` |

**约束**：shell 集成脚本在用户 shell 中运行，不只是构建期。Bash heredoc 生成 zsh 时必须转义反引号和 `$(...)`。

---

## 5. 命令面板与浮层

| 编号 | 需求 | 状态 | 验证 |
|---|---|---|---|
| U-101 | 命令面板：按名称/描述检索并执行任意 `KeyAssignment` | 已实现 | `overlay/launcher.rs` |
| U-102 | QuickSelect：屏幕文本快速选中 | 已实现 | `overlay/quickselect.rs` |
| U-103 | 字符/Emoji 选择器 | 已实现 | `charselect.rs` |
| U-104 | 通用确认浮层与输入提示浮层 | 已实现 | `overlay/confirm.rs`、`overlay/prompt.rs` |
| U-105 | 浮层全屏独占；浮层期间按键不进底层 pane | 已实现 | `get_active_pane_or_overlay` |

---

## 6. ZMODEM 文件传输

### 6.1 已实现能力

| 编号 | 需求 | 状态 | 验证 |
|---|---|---|---|
| Z-101 | 检测远端 `rz`/`sz` 发起的传输会话 | 已实现 | `mux/src/zmodem/detect.rs` |
| Z-102 | 文件选择器挑选发送文件 / 保存目录；空格或 Ctrl+S 确认目录 | 已实现 | `overlay/filebrowser.rs` |
| Z-103 | 取消选择时向远端发送中止，并立即允许再次发起 | 已实现 | `finish_cancelled`，单测覆盖 |
| Z-104 | 传输期间 PTY 读取权独占，另一读者阻塞等待 | 已实现 | `ReadGate`，单测覆盖 |
| Z-105 | 传输期间拒绝所有非传输写入（按键、粘贴、注入、图片粘贴、字符写入） | 已实现 | `is_zmodem_exclusive`，6 条路径全覆盖 |
| Z-106 | 被拒绝的按键回显到屏幕（不写入 PTY） | 已实现 | `key_echo`，单测覆盖 |
| Z-107 | 上传时以 `sz -e` 运行，转义全部控制字节 | 已实现 | 单测 `escape_flag_tests` |
| Z-108 | PTY 特殊字符槽清空，`VMIN`/`VTIME` 保留 | 已实现 | `prepare_transfer_tty`，pty 实测 |
| Z-109 | 停滞检测：30 秒无字节移动才放弃，不限制总时长 | 已实现 | `STALL_TIMEOUT`，单测覆盖 |
| Z-110 | 传输结束清空 PTY 残留，防止被当作新会话检测 | 已实现 | `zmodem_finish` + `drain` |
| Z-111 | 失败时向远端发送中止序列 | 已实现 | `CANCEL_SEQUENCE` |
| Z-112 | 传输进度显示 | 已实现 | `zmodem/progress.rs` |

### 6.2 已知问题

| 编号 | 问题 | 状态 |
|---|---|---|
| Z-901 | 大文件上传在真实 ssh 链路上仍会失败：远端反复请求重传（`ZRPOS`/`NAK on sector`），最终中止 | **未解决** |
| Z-902 | 上传失败后的残留字节可能被 ssh 解释为转义序列（如 `~v`），导致会话 Debug 级别被提升 | 已缓解（`sz -e` 后应不再出现），未复测 |

**Z-901 的排除项**（均已实测排除）：
- PTY 本身不丢字节、不改字节（含 `0x0f`/`0x16` 场景）
- relay 两方向均为纯拷贝，无过滤/重排
- 本地等价拓扑（pty + 40ms 延迟）可完整传完 49 MB

---

## 7. SSH 与远程

| 编号 | 需求 | 状态 | 验证 |
|---|---|---|---|
| R-101 | 通过 ssh 连接远程主机的 domain | 已实现 | `mux/src/ssh.rs` |
| R-102 | SSH agent 转发 | 已实现 | `mux/src/ssh_agent.rs` |
| R-103 | tmux 集成（attach / detach / 控制模式） | 已实现 | `mux/src/tmux.rs` |
| R-104 | 远程文件浏览（Remote Files） | 已实现 | `docs/features.md` |

---

## 8. CLI

| 编号 | 命令 | 状态 |
|---|---|---|
| L-101 | `faz`（无参数）：启动或连接已有实例 | 已实现 |
| L-102 | `faz start` / `faz blocking-start` | 已实现 |
| L-103 | `faz config`：交互式配置 TUI | 已实现 |
| L-104 | `faz init`：安装 shell 集成 | 已实现 |
| L-105 | `faz doctor`：环境与运行时诊断 | 已实现 |
| L-106 | `faz reset`：移除 shell 集成与托管默认值 | 已实现 |
| L-107 | `faz cli`：在指定 pane 中执行命令 | 已实现 |
| L-108 | `faz set-cwd`：同步 pane 工作目录 | 已实现 |

**已移除**：`faz update`。应用内无更新入口，更新走 Homebrew cask 或源码构建。

---

## 9. macOS 集成

| 编号 | 需求 | 状态 | 验证 |
|---|---|---|---|
| M-101 | 原生菜单栏，五个顶层菜单 | 已实现 | `faz-gui/src/commands.rs` |
| M-102 | 显示/隐藏、最小化、全屏、关闭行为 | 已实现 | Window 菜单 |
| M-103 | 设为默认终端 | 已实现 | Faz 菜单 |
| M-104 | 通知中心集成（首次绘制后初始化，不阻塞启动） | 已实现 | `termwindow/render/paint.rs` |
| M-105 | 最小化 AppleScript 字典（只读属性 + quit） | 已实现 | `docs/features.md` |
| M-106 | 仅构建 Apple Silicon（arm64） | 已实现 | `scripts/build.sh` |

**约束**：
- AppKit 菜单 `keyEquivalent` 会在 `keyDown` 之前拦截按键。`Ctrl+<letter>` 在 TUI 内失效时先查菜单，再查 termwiz。
- 菜单栏初始化时序敏感，改动需带回归测试。

---

## 10. 构建与发布

| 编号 | 需求 | 状态 | 验证 |
|---|---|---|---|
| B-101 | `make fmt` / `fmt-check` / `check` / `test` | 已实现 | `Makefile` |
| B-102 | `make app` 构建可运行的 app bundle | 已实现 | `scripts/build.sh` |
| B-103 | CI：fmt、check、test、日志与提示守卫 | 已实现 | `.github/workflows/checks.yml` |
| B-104 | 发布形态构建（universal / bundle） | 已实现 | `.github/workflows/build-validation.yml` |
| B-105 | 发布说明脚本与格式校验 | 已实现 | `scripts/check_release_notes.sh` |
| B-106 | 配置发布就绪校验 | 已实现 | `scripts/check_config_release_readiness.sh` |

---

## 11. 明确不做

以下能力经产品决策排除，不应重新引入：

| 编号 | 排除项 | 原因 |
|---|---|---|
| N-101 | UI 多语言 / `config.language` | `rust-i18n` 方案已回退，UI 保持英文字面量 |
| N-102 | 应用内更新（菜单、后台检查、`faz update`） | 更新归 Homebrew cask |
| N-103 | Help 菜单与 GitHub 快捷命令 | 已移除 |
| N-104 | AppleScript `do script` | 不向 AppleScript 暴露 shell 执行 |

---

## 附：需求与代码的对应关系

| 功能域 | 主要位置 |
|---|---|
| 终端语义 | `term/`、`termwiz/` |
| 窗口与 pane | `mux/`、`window/` |
| GUI 渲染与输入 | `faz-gui/src/termwindow/` |
| 配置 | `config/`、`lua-api-crates/` |
| ZMODEM | `mux/src/zmodem/` |
| CLI | `faz-cli/` |
| Shell 集成 | `assets/shell-integration/` |
| 构建发布 | `scripts/`、`.github/workflows/` |

---

*本文档随功能变更更新。新增或移除功能时，同步修改对应条目及其状态。*
