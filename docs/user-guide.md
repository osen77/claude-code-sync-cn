# Claude Code Sync 用户指南

本文档包含 `ccs` 的安装配置、多设备同步和常用示例。

---

## 目录

- [安装与更新](#安装与更新)
- [多设备同步配置](#多设备同步配置)
- [日常使用](#日常使用)
- [自动同步（推荐）](#自动同步推荐)
- [配置同步](#配置同步)
- [会话管理](#会话管理)
- [常用命令示例](#常用命令示例)
- [高级配置](#高级配置)
- [故障排查](#故障排查)
- [卸载](#卸载)

---

## 安装与更新

### 一键安装（推荐）

直接下载最新版预编译二进制，无需额外依赖：

```bash
# macOS Apple Silicon (M1/M2/M3/M4)
curl -fsSL https://github.com/osen77/claude-code-sync-cn/releases/latest/download/ccs-macos-aarch64.tar.gz | tar xz && sudo mv ccs /usr/local/bin/

# macOS Intel
curl -fsSL https://github.com/osen77/claude-code-sync-cn/releases/latest/download/ccs-macos-x86_64.tar.gz | tar xz && sudo mv ccs /usr/local/bin/

# Linux x86_64
curl -fsSL https://github.com/osen77/claude-code-sync-cn/releases/latest/download/ccs-linux-x86_64.tar.gz | tar xz && sudo mv ccs /usr/local/bin/
```

> **不确定你的 Mac 芯片？** 运行 `uname -m`，输出 `arm64` 是 Apple Silicon，`x86_64` 是 Intel。

### 安装脚本

自动检测平台并安装：

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/osen77/claude-code-sync-cn/main/install.sh | bash

# Windows PowerShell
irm https://raw.githubusercontent.com/osen77/claude-code-sync-cn/main/install.ps1 | iex
```

### 从源码安装

```bash
git clone https://github.com/osen77/claude-code-sync-cn
cd claude-code-sync
cargo install --path .
```

### 更新

```bash
# 方式一：内置更新命令
ccs update

# 方式二：重新下载覆盖（适用于旧版本无 update 命令的情况）
curl -fsSL https://github.com/osen77/claude-code-sync-cn/releases/latest/download/ccs-macos-aarch64.tar.gz | tar xz && sudo mv ccs $(which ccs)
```

> 将 URL 中的 `ccs-macos-aarch64` 替换为你的平台：`ccs-macos-x86_64`（Intel Mac）、`ccs-linux-x86_64`（Linux）。

---

## 多设备同步配置

### 前置条件

- 已创建 GitHub 私有仓库（如 `claude-code-history`）
- 已在所有设备上安装 `ccs`
- 已配置 Git 认证（推荐使用 `gh auth login`）

### 设备 A（首次设置）

```bash
# 运行配置向导
ccs setup
```

向导会引导你：
1. 选择同步模式（多设备/单设备）
2. 输入或创建远程仓库
3. 设置本地备份目录
4. 设置过滤选项（排除附件、旧对话）
5. 可选执行首次同步
6. 配置自动同步（推荐）- 启动时自动拉取，退出时自动推送
7. 配置跨设备配置同步

### 设备 B（加入同步）

```bash
# 运行配置向导，选择已有仓库
ccs setup
```

或手动初始化：

```bash
ccs init \
  --local ~/claude-history-backup \
  --remote https://github.com/YOUR_USERNAME/claude-code-history.git \
  --clone
```

### 验证配置

```bash
# 确认显示 "Use project name only: Yes"
ccs config --show

# 查看状态
ccs status
```

---

## 日常使用

### 推荐：sync 命令

```bash
# 开始/结束工作时执行
ccs sync
```

`sync` 命令会自动：
1. 拉取远程更新 (pull)
2. 合并本地变更
3. 推送到远程 (push)

### 分步操作

```bash
# 仅拉取
ccs pull

# 仅推送
ccs push -m "Update from Mac"
```

### 切换设备工作流

**在设备 A 结束工作：**
```bash
ccs push -m "Windows session"
```

**在设备 B 开始工作：**
```bash
ccs pull
```

---

## 自动同步（推荐）

自动同步可以免去手动执行 `push`/`pull` 的麻烦。

### 配置方式

**方式一：通过 setup 向导（新用户推荐）**

```bash
ccs setup
```

向导最后会询问是否配置自动同步，选择"是"即可一键完成所有配置。

**方式二：单独配置（已完成 setup 的用户）**

```bash
ccs automate
```

此命令会：
1. 安装 Claude Code Hooks（退出时自动推送）
2. 创建启动包装脚本（启动时自动拉取）

### 使用方式

配置完成后，使用 `claude-sync` 替代 `claude` 启动 Claude Code：

```bash
# 使用包装脚本启动（推荐）
claude-sync

# 或添加别名到 shell 配置文件（~/.bashrc 或 ~/.zshrc）
alias claude='claude-sync'
```

### 自动同步流程

```
启动时: claude-sync → 自动 pull → 启动 Claude Code
使用中: 检测新项目 → 自动 pull 该项目历史
每轮对话结束: Stop Hook → 自动 push
```

### 管理命令

```bash
# 查看自动同步状态
ccs automate --status

# 卸载自动同步
ccs automate --uninstall

# 单独管理 hooks
ccs hooks install    # 安装 hooks
ccs hooks uninstall  # 卸载 hooks
ccs hooks show       # 查看状态

# 单独管理包装脚本
ccs wrapper install    # 创建 claude-sync
ccs wrapper uninstall  # 删除 claude-sync
ccs wrapper show       # 查看状态
```

### Hooks 说明

| Hook | 触发时机 | 功能 |
|------|----------|------|
| `SessionStart` | Claude Code 首次启动时 | 拉取最新历史（三重条件检测） |
| `Stop` | 每轮对话完成后 | 推送对话历史 |
| `UserPromptSubmit` | 每次发送消息时 | 检测新项目并拉取远程历史 |

> **SessionStart 三重条件检测**：只有同时满足以下条件才会执行 pull：
> 1. 进程数 = 1（没有其他 Claude 实例）
> 2. source = "startup"（不是 resume/compact）
> 3. 5分钟内未触发过（防抖保护）
>
> 这确保了 `/new`、新窗口、对话压缩等场景不会重复拉取。详见 [Hooks 避坑指南](claude-code-hooks-guide.md)。

### 调试

如果自动同步未生效，检查调试日志：

```bash
# macOS
cat ~/Library/Application\ Support/claude-code-sync/hook-debug.log

# Linux
cat ~/.config/claude-code-sync/hook-debug.log
```

---

## 配置同步

除了对话历史，`ccs` 还支持同步 Claude Code 配置文件，让你在多个设备间保持一致的使用体验。

### 同步内容

| 文件 | 默认同步 | 说明 |
|------|---------|------|
| `settings.json` | ✅ | 权限、模型配置（自动过滤 hooks 字段） |
| `CLAUDE.md` | ✅ | 用户全局指令（支持平台标签） |
| `installed_skills.json` | ✅ | 已安装 skills 列表 |
| `hooks/` | ❌ | 默认不同步（路径兼容问题） |

### 基本命令

```bash
# 推送当前设备配置到远程
ccs config-sync push

# 查看远程所有设备配置
ccs config-sync list

# 应用其他设备的配置
ccs config-sync apply MacBook-Pro

# 查看配置同步状态
ccs config-sync status
```

### 平台标签

CLAUDE.md 中可能包含平台特定内容。使用平台标签标记后，跨平台应用时会自动过滤。

**标签格式：**

```markdown
# 通用内容（所有平台共享）

## 通用规范
- 代码规范...

<!-- platform:macos -->
## macOS 环境
- 使用 fnm 管理 node 版本
- Homebrew 路径: /opt/homebrew/bin
<!-- end-platform -->

<!-- platform:windows -->
## Windows 环境
- 使用 nvm-windows 管理 node 版本
- 路径分隔符使用反斜杠
<!-- end-platform -->

<!-- platform:linux -->
## Linux 环境
- 使用 nvm 管理 node 版本
<!-- end-platform -->
```

**支持的标签：**

| 标签 | 别名 | 平台 |
|------|------|------|
| `macos` | `mac`, `darwin` | macOS |
| `windows` | `win` | Windows |
| `linux` | - | Linux |

### 应用配置示例

**场景：** 在 Windows 上应用来自 Mac 的配置

```bash
# 查看可用设备
ccs config-sync list
# 输出: MacBook-Pro, Windows-PC

# 应用 Mac 配置
ccs config-sync apply MacBook-Pro
```

**结果：**
- `settings.json` 完整应用（hooks 字段自动过滤）
- `CLAUDE.md` 保留通用内容 + 保留本地 Windows 平台块
- macOS 平台块内容被过滤

### 设备名称

配置按设备名存储在仓库的 `_configs/<device>/` 目录下。

设备名获取优先级：
- **macOS**: 系统偏好设置中的「电脑名称」
- **Windows**: COMPUTERNAME 环境变量
- **Linux**: /etc/hostname

如果名称包含中文或特殊字符，会自动替换为 `-`。

### 目录结构

```
sync-repo/
├── _configs/                    # 配置同步目录
│   ├── MacBook-Pro/
│   │   ├── settings.json
│   │   ├── CLAUDE.md
│   │   └── installed_skills.json
│   └── Windows-PC/
│       └── ...
│
└── projects/                    # 对话历史目录
    └── ...
```

---

## 会话管理

`ccs` 提供交互式会话管理功能，可以查看、重命名和删除 Claude Code 对话会话。

### 交互模式（推荐）

```bash
# 进入交互式界面
ccs session
```

**在项目目录中运行：**
- 自动识别当前目录对应的项目
- 直接显示该项目的会话列表

**在非项目目录中运行：**
- 显示所有项目列表供选择
- 选择项目后进入该项目的会话列表

**导航操作：**
- 选择会话 → 进入操作菜单（详情/重命名/删除）
- 选择「← 切换到其他项目」→ 返回项目列表
- 选择「✕ 退出」→ 退出程序
- 操作完成后可返回上一级继续操作

### 非交互模式

```bash
# 列出所有项目和会话数量
ccs session list

# 列出特定项目的会话
ccs session list --project my-project

# 显示完整会话 ID
ccs session list --show-ids

# 查看会话详情
ccs session show <session-id>

# 重命名会话
ccs session rename <session-id> "新的标题"

# 删除会话（需确认）
ccs session delete <session-id>

# 强制删除（跳过确认）
ccs session delete <session-id> --force
```

### 会话标题

会话标题取自第一条真实的用户消息。以下内容会被自动过滤：
- IDE 自动发送的 `<ide_opened_file>` 标签
- IDE 自动发送的 `<ide_selection>` 标签
- 系统预热消息 `Warmup`

### 示例输出

```
📂 检测到当前项目: my-project
找到 5 个会话

> 1. 帮我实现用户认证功能...          12条消息  今天
  2. 修复登录页面的样式问题...         8条消息  昨天
  3. 重构数据库连接池...              25条消息  3天前
  ─────────────────────────────────────────────────
  ← 切换到其他项目
  ✕ 退出
```

---

## 常用命令示例

### 基本操作

| 命令 | 说明 |
|------|------|
| `ccs setup` | 交互式配置向导 |
| `ccs sync` | 双向同步 |
| `ccs pull` | 拉取远程更新 |
| `ccs push` | 推送本地更新 |
| `ccs status` | 查看同步状态 |
| `ccs automate` | 配置自动同步 |
| `ccs session` | 交互式会话管理 |
| `ccs session list` | 列出所有会话 |
| `ccs session show <id>` | 查看会话详情 |
| `ccs session rename <id> <title>` | 重命名会话 |
| `ccs session delete <id>` | 删除会话 |
| `ccs config-sync push` | 推送配置到远程 |
| `ccs config-sync list` | 列出远程设备配置 |
| `ccs config-sync apply <device>` | 应用其他设备配置 |
| `ccs config-sync status` | 查看配置同步状态 |
| `ccs hooks show` | 查看 hooks 状态 |
| `ccs wrapper show` | 查看包装脚本状态 |
| `ccs update` | 更新到最新版本 |
| `ccs uninstall` | 卸载并清理所有数据 |

### 配置管理

```bash
# 查看当前配置
ccs config --show

# 只同步最近 30 天的对话
ccs config --exclude-older-than 30

# 排除特定项目
ccs config --exclude-projects "*test*,*temp*"

# 只同步特定项目
ccs config --include-projects "*work*,*important*"
```

### 状态检查

```bash
# 基本状态
ccs status

# 显示文件列表
ccs status --show-files

# 查看冲突
ccs status --show-conflicts
```

### 冲突报告

```bash
# 生成 Markdown 报告
ccs report --format markdown

# 生成 JSON 报告并保存
ccs report --format json --output conflicts.json
```

---

## 高级配置

### Git LFS（大文件）

```bash
# 启用 LFS
ccs config --enable-lfs true

# 自定义 LFS 模式
ccs config --enable-lfs true --lfs-patterns "*.jsonl,*.png"
```

### 自定义同步目录

```bash
# 更改存储子目录（默认 "projects"）
ccs config --sync-subdirectory "claude-conversations"
```

### 自动化备份

**macOS/Linux crontab：**
```bash
# 每天晚上 11 点同步
0 23 * * * ~/.local/bin/ccs sync
```

**非交互式初始化：**

创建 `~/.claude-code-sync-init.toml`：
```toml
repo_path = "~/claude-history-sync"
remote_url = "git@github.com:user/claude-history.git"
clone = true
use_project_name_only = true
```

运行：
```bash
ccs init --config ~/.claude-code-sync-init.toml
```

### 命令别名

**Bash/Zsh：**
```bash
alias ccs='ccs'
alias ccs-sync='ccs sync'
```

**PowerShell：**
```powershell
Set-Alias ccs ccs
```

---

## 故障排查

### 问题 1：No matching local project found

**原因：** 本地没有该项目或路径解析失败

**解决：**
1. 在本地用 Claude Code 打开该项目
2. 确保 `use_project_name_only = true` 已配置
3. 重新执行 `ccs pull`

### 问题 2：Authentication failed

**解决：**
```bash
# 使用 GitHub CLI 认证
gh auth login

# 或配置 SSH key
ssh-keygen -t ed25519
cat ~/.ssh/id_ed25519.pub  # 添加到 GitHub
```

### 问题 3：冲突处理

**自动处理：**
- 冲突文件会保留两个版本
- 远程版本：`session.jsonl`
- 本地版本：`session-conflict-<timestamp>.jsonl`

**手动解决：**
1. 查看冲突报告：`ccs report`
2. 选择需要保留的版本
3. 删除不需要的文件
4. 推送：`ccs push`

### 问题 4：更新失败

```bash
# 检查更新
ccs update --check-only

# 自动更新
ccs update

# 如果 update 命令不可用（旧版本），直接下载替换：
curl -fsSL https://github.com/osen77/claude-code-sync-cn/releases/latest/download/ccs-macos-aarch64.tar.gz | tar xz && sudo mv ccs $(which ccs)
```

---

## 卸载

```bash
# 交互式卸载（逐步确认清理范围）
ccs uninstall

# 强制卸载（跳过确认）
ccs uninstall --force
```

卸载会清理：
1. Claude Code hooks（从 `~/.claude/settings.json` 移除）
2. 启动包装脚本（`claude-sync`）
3. 配置目录（state.json、config.toml、日志等）
4. 同步仓库（需单独确认，可能包含未推送的对话历史）
5. ccs 二进制本身（需单独确认）

---

## 配置文件位置

| 平台 | 配置文件 |
|------|---------|
| Windows | `%APPDATA%\claude-code-sync\config.toml` |
| macOS | `~/Library/Application Support/claude-code-sync/config.toml` |
| Linux | `~/.config/claude-code-sync/config.toml` |

---

## 重要注意事项

### 项目名称一致性

确保不同设备上的项目文件夹名称相同：
- ✅ Windows `C:\Projects\my-app`，Mac `/Users/mini/Projects/my-app`
- ❌ Windows `C:\work\app1`，Mac `/Users/mini/code/myapp`

### 同步时机

- **开始工作前**：`pull` 或 `sync`
- **结束工作后**：`push` 或 `sync`
- **切换设备时**：先 push，再到新设备 pull

---

## 相关资源

- **仓库**: https://github.com/osen77/claude-code-sync-cn
- **问题追踪**: https://github.com/osen77/claude-code-sync-cn/issues
- **上游项目**: https://github.com/perfectra1n/claude-code-sync

---

*最后更新: 2026-03-26*
