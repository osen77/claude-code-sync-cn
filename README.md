# claude-code-sync

[![Release](https://github.com/osen77/claude-code-sync-cn/actions/workflows/release.yml/badge.svg)](https://github.com/osen77/claude-code-sync-cn/actions/workflows/release.yml)

一个用于同步 Claude Code 对话历史的 Rust CLI 工具，支持跨设备备份和版本控制。

![Demo](demo1.svg)

## 文档

📚 **[用户指南](docs/user-guide.md)** - 安装配置、多设备同步、常用命令

📚 **[API 文档](https://perfectra1n.github.io/claude-code-sync/)** - 代码 API 参考

本地构建文档：
```bash
cargo doc --open --no-deps --all-features
```

## 功能特性

| 功能 | 说明 |
|------|------|
| **智能合并** | 自动合并非冲突的对话变更 |
| **双向同步** | `sync` 命令一键完成拉取和推送 |
| **交互式配置** | 首次运行向导引导完成配置 |
| **自动更新** | 启动时检查新版本，支持一键更新 |
| **非交互式初始化** | 支持配置文件，适用于 CI/CD |
| **智能冲突解决** | 交互式 TUI 预览和解决冲突 |
| **选择性同步** | 按项目、日期过滤，排除附件 |
| **Git LFS 支持** | 高效存储大文件 |
| **Mercurial 支持** | 可选使用 Mercurial 替代 Git |
| **撤销操作** | 自动快照，支持回滚 pull/push |
| **操作历史** | 追踪和查看历史同步记录 |
| **分支管理** | 同步到不同分支，管理远程仓库 |
| **详细日志** | 控制台和文件日志，可配置级别 |
| **冲突报告** | JSON/Markdown 格式的冲突报告 |
| **灵活配置** | 基于 TOML 的配置，支持 CLI 覆盖 |

## 概述

`claude-code-sync` 将 Claude Code 对话历史同步到 Git 仓库，实现：

- **备份**：永不丢失 Claude Code 对话
- **多设备同步**：在多台电脑间保持对话历史一致
- **版本控制**：追踪对话历史的变更
- **冲突解决**：自动处理不同设备上的历史分歧

## 工作原理

Claude Code 将对话历史存储在 `~/.claude/projects/` 目录下的 JSONL 文件中。每个项目有独立目录，每个对话是一个 `.jsonl` 文件。

`claude-code-sync` 的工作流程：
1. 发现本地 Claude Code 历史中的所有对话文件
2. 复制到 Git 仓库
3. 提交并可选推送到远程
4. 拉取时，合并远程变更到本地历史
5. 检测冲突（同一会话在不同设备上被修改）
6. 通过重命名文件保留两个版本来解决冲突

## 安装

### 一键安装（推荐）

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/osen77/claude-code-sync-cn/main/install.sh | bash

# Windows PowerShell
irm https://raw.githubusercontent.com/osen77/claude-code-sync-cn/main/install.ps1 | iex
```

安装脚本会自动下载预编译二进制文件并运行配置向导。

### 从源码安装

```bash
git clone https://github.com/osen77/claude-code-sync-cn
cd claude-code-sync
cargo install --path .
```

## 快速开始

### 首次配置（交互式向导）

```bash
# 运行配置向导
claude-code-sync setup
```

向导会引导你：
- 选择同步模式（多设备/单设备）
- 输入或创建远程仓库
- 设置本地备份目录
- 可选执行首次同步

### 日常使用

```bash
# 双向同步（推荐）
claude-code-sync sync

# 仅拉取远程更新
claude-code-sync pull

# 仅推送本地变更
claude-code-sync push -m "Daily backup"
```

### 检查更新

```bash
# 检查新版本
claude-code-sync update --check-only

# 更新到最新版本
claude-code-sync update
```

## 命令参考

### `setup`

交互式配置向导，首次使用推荐。

```bash
claude-code-sync setup
```

### `init`

初始化同步仓库。

```bash
claude-code-sync init --repo <路径> [--remote <URL>] [--clone]
```

**选项：**
- `--repo, -r <路径>`：存储历史的 Git 仓库路径
- `--remote <URL>`：远程 Git URL
- `--clone`：从远程克隆仓库
- `--config <路径>`：从 TOML 文件加载配置

**示例：**
```bash
claude-code-sync init --repo ~/claude-backup --remote git@github.com:user/claude-history.git --clone
```

#### 非交互式初始化（CI/CD）

创建配置文件 `~/.claude-code-sync-init.toml`：

```toml
repo_path = "~/claude-history-sync"
remote_url = "https://github.com/user/repo.git"
clone = true
exclude_attachments = true
use_project_name_only = true
```

运行：
```bash
claude-code-sync init --config ~/.claude-code-sync-init.toml
```

### `sync`

双向同步（先拉取后推送）。

```bash
claude-code-sync sync [选项]
```

**选项：**
- `--message, -m <消息>`：提交信息
- `--branch, -b <分支>`：同步的分支（默认：当前分支）
- `--exclude-attachments`：仅同步 .jsonl 文件

**示例：**
```bash
claude-code-sync sync -m "Daily sync" --exclude-attachments
```

### `push`

推送本地历史到同步仓库。

```bash
claude-code-sync push [选项]
```

**选项：**
- `--message, -m <消息>`：提交信息
- `--push-remote`：提交后推送到远程（默认：true）
- `--branch, -b <分支>`：推送的分支
- `--exclude-attachments`：仅同步 .jsonl 文件

### `pull`

从同步仓库拉取并合并历史。

```bash
claude-code-sync pull [选项]
```

**选项：**
- `--fetch-remote`：合并前从远程拉取（默认：true）
- `--branch, -b <分支>`：拉取的分支

### `status`

显示同步状态和信息。

```bash
claude-code-sync status [--show-conflicts] [--show-files]
```

**选项：**
- `--show-conflicts`：显示详细冲突信息
- `--show-files`：显示将要同步的文件

### `config`

配置同步过滤器和设置。

```bash
claude-code-sync config [选项] [--show]
```

**选项：**
- `--exclude-older-than <天数>`：排除超过 N 天的项目
- `--include-projects <模式>`：仅包含特定项目（逗号分隔）
- `--exclude-projects <模式>`：排除特定项目（逗号分隔）
- `--exclude-attachments <true|false>`：排除附件
- `--enable-lfs <true|false>`：启用 Git LFS
- `--scm-backend <后端>`：SCM 后端：`git` 或 `mercurial`
- `--sync-subdirectory <目录>`：同步仓库中的子目录（默认：`projects`）
- `--show`：显示当前配置

**示例：**
```bash
# 排除超过 30 天的对话
claude-code-sync config --exclude-older-than 30

# 仅包含特定项目
claude-code-sync config --include-projects "*my-project*,*important*"

# 启用 Git LFS
claude-code-sync config --enable-lfs true

# 显示当前配置
claude-code-sync config --show
```

### `update`

检查并更新到最新版本。

```bash
claude-code-sync update [--check-only]
```

**选项：**
- `--check-only`：仅检查，不执行更新

### `report`

查看冲突报告。

```bash
claude-code-sync report [--format <格式>] [--output <文件>]
```

**选项：**
- `--format, -f <格式>`：输出格式：`json`、`markdown` 或 `text`
- `--output, -o <文件>`：输出文件（默认：打印到控制台）

### `remote`

管理 Git 远程配置。

```bash
claude-code-sync remote <命令>
```

**命令：**
- `show`：显示当前远程配置
- `set`：设置或更新远程 URL
- `remove`：移除远程

**示例：**
```bash
# 显示当前远程
claude-code-sync remote show

# 设置远程 URL
claude-code-sync remote set origin https://github.com/user/repo.git
```

### `undo`

撤销上次同步操作。

```bash
claude-code-sync undo <操作>
```

**操作：**
- `pull`：撤销上次拉取
- `push`：撤销上次推送

**工作原理：**
- 每次 pull/push 操作自动创建快照
- 快照存储在 `~/.claude-code-sync/snapshots/`
- 撤销后快照自动删除

### `history`

查看和管理操作历史。

```bash
claude-code-sync history <命令>
```

**命令：**
- `list`：列出最近的同步操作
- `last`：显示上次操作的详细信息
- `clear`：清除所有操作历史

**示例：**
```bash
# 列出最近 10 次操作
claude-code-sync history list

# 显示上次操作详情
claude-code-sync history last

# 清除历史
claude-code-sync history clear
```

## 冲突解决

### 智能合并（默认）

检测到冲突时，自动尝试智能合并：

- **分析消息 UUID 和父关系**：构建消息树理解对话结构
- **按时间戳解决编辑冲突**：同一消息被编辑时保留较新版本
- **保留所有对话分支**：对话分歧时保留所有分支

**智能合并自动处理：**
- ✅ 非重叠变更（简单合并）
- ✅ 对话不同部分的消息添加
- ✅ 对话分支（同一点的多个延续）
- ✅ 编辑的消息（按时间戳解决）

### 交互式冲突解决

在交互式终端中，提供 TUI 界面解决冲突：

- 📋 列出所有冲突
- 🔍 预览本地和远程版本差异
- 📊 查看统计：消息数、时间戳、文件大小
- 🎯 选择解决方式：智能合并、保留本地、保留远程、保留两者

### 自动解决（非交互式）

非交互式环境中，冲突自动解决：

- 本地版本保持不变
- 远程版本保存为 `-conflict-<时间戳>.jsonl`

## 配置文件

配置存储在 `~/.claude-code-sync.toml`：

```toml
# 排除超过 N 天的项目
exclude_older_than_days = 30

# 仅包含这些项目模式
include_patterns = ["*my-project*", "*work*"]

# 排除这些项目模式
exclude_patterns = ["*test*", "*temp*"]

# 最大文件大小（字节，默认 10MB）
max_file_size_bytes = 10485760

# 排除附件
exclude_attachments = false

# 启用 Git LFS
enable_lfs = false

# SCM 后端："git" 或 "mercurial"
scm_backend = "git"

# 同步仓库中的子目录
sync_subdirectory = "projects"

# 仅使用项目名（多设备模式）
use_project_name_only = true
```

## 同步状态

同步状态存储在 `~/.claude-code-sync/`：
- `state.json`：当前同步仓库配置
- `operation-history.json`：操作历史（最多 5 条）
- `snapshots/`：撤销快照目录
- `latest-conflict-report.json`：最新冲突报告

## 使用场景

### 每日备份

```bash
# 每天结束时
claude-code-sync push -m "Daily backup $(date +%Y-%m-%d)"
```

### 多设备开发

**设备 A：**
```bash
claude-code-sync setup
claude-code-sync push
```

**设备 B：**
```bash
claude-code-sync setup  # 选择相同的远程仓库
claude-code-sync pull
# 在设备 B 上工作
claude-code-sync push
```

**回到设备 A：**
```bash
claude-code-sync pull  # 合并设备 B 的变更
```

### 自动化备份（Cron）

```bash
# 每晚 11 点同步
0 23 * * * ~/.local/bin/claude-code-sync sync >> ~/claude-code-sync.log 2>&1
```

## 日志

### 控制台日志

使用 `RUST_LOG` 环境变量控制：

```bash
# 显示调试信息
RUST_LOG=debug claude-code-sync sync

# 仅显示错误
RUST_LOG=error claude-code-sync push

# 禁用控制台输出
RUST_LOG=off claude-code-sync status
```

**日志级别：** `trace` > `debug` > `info` > `warn` > `error` > `off`

### 文件日志

所有操作自动记录到文件：

- **Linux**: `~/.config/claude-code-sync/claude-code-sync.log`
- **macOS**: `~/Library/Application Support/claude-code-sync/claude-code-sync.log`
- **Windows**: `%APPDATA%\claude-code-sync\claude-code-sync.log`

## 故障排查

### "Sync not initialized"

运行 `claude-code-sync setup` 或 `claude-code-sync init` 进行初始化。

### "Failed to push to remote"

检查：
- Git 远程 URL 是否正确
- SSH 密钥或凭据是否配置
- 网络连接
- 远程仓库权限

### 每次拉取都有冲突

可能原因：
- 设备间时钟不同步
- 过滤器配置不同
- 多台设备同时使用相同对话

## 安全考虑

- 对话历史可能包含敏感信息
- 建议使用私有 Git 仓库
- 考虑加密 Git 仓库以增强安全性
- 推荐使用 SSH 密钥或访问令牌进行 Git 认证

## 相关资源

- **中文仓库**: https://github.com/osen77/claude-code-sync-cn
- **上游项目**: https://github.com/perfectra1n/claude-code-sync
- **问题追踪**: https://github.com/osen77/claude-code-sync-cn/issues

## 贡献

欢迎贡献！请：
1. Fork 仓库
2. 创建功能分支
3. 为新功能添加测试
4. 提交 Pull Request

---

*最后更新: 2026-02-02*
