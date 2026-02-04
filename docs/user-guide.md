# Claude Code Sync 用户指南

本文档包含 `claude-code-sync` 的安装配置、多设备同步和常用示例。

---

## 目录

- [快速安装](#快速安装)
- [多设备同步配置](#多设备同步配置)
- [日常使用](#日常使用)
- [自动同步（推荐）](#自动同步推荐)
- [配置同步](#配置同步)
- [常用命令示例](#常用命令示例)
- [高级配置](#高级配置)
- [故障排查](#故障排查)

---

## 快速安装

### 一键安装（推荐）

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/osen77/claude-code-sync-cn/main/install.sh | bash

# Windows PowerShell
irm https://raw.githubusercontent.com/osen77/claude-code-sync-cn/main/install.ps1 | iex
```

安装脚本会：
1. 下载预编译二进制文件
2. 添加到 PATH
3. 运行交互式配置向导 (`setup`)

### 从源码安装

```bash
git clone https://github.com/osen77/claude-code-sync-cn
cd claude-code-sync
cargo install --path .
```

---

## 多设备同步配置

### 前置条件

- 已创建 GitHub 私有仓库（如 `claude-code-history`）
- 已在所有设备上安装 `claude-code-sync`
- 已配置 Git 认证（推荐使用 `gh auth login`）

### 设备 A（首次设置）

```bash
# 运行配置向导
claude-code-sync setup
```

向导会引导你：
1. 选择同步模式（多设备/单设备）
2. 输入或创建远程仓库
3. 设置本地备份目录
4. 可选执行首次同步
5. 配置自动同步（推荐）- 启动时自动拉取，退出时自动推送

### 设备 B（加入同步）

```bash
# 运行配置向导，选择已有仓库
claude-code-sync setup
```

或手动初始化：

```bash
claude-code-sync init \
  --local ~/claude-history-backup \
  --remote https://github.com/YOUR_USERNAME/claude-code-history.git \
  --clone
```

### 验证配置

```bash
# 确认显示 "Use project name only: Yes"
claude-code-sync config --show

# 查看状态
claude-code-sync status
```

---

## 日常使用

### 推荐：sync 命令

```bash
# 开始/结束工作时执行
claude-code-sync sync
```

`sync` 命令会自动：
1. 拉取远程更新 (pull)
2. 合并本地变更
3. 推送到远程 (push)

### 分步操作

```bash
# 仅拉取
claude-code-sync pull

# 仅推送
claude-code-sync push -m "Update from Mac"
```

### 切换设备工作流

**在设备 A 结束工作：**
```bash
claude-code-sync push -m "Windows session"
```

**在设备 B 开始工作：**
```bash
claude-code-sync pull
```

---

## 自动同步（推荐）

自动同步可以免去手动执行 `push`/`pull` 的麻烦。

### 配置方式

**方式一：通过 setup 向导（新用户推荐）**

```bash
claude-code-sync setup
```

向导最后会询问是否配置自动同步，选择"是"即可一键完成所有配置。

**方式二：单独配置（已完成 setup 的用户）**

```bash
claude-code-sync automate
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
claude-code-sync automate --status

# 卸载自动同步
claude-code-sync automate --uninstall

# 单独管理 hooks
claude-code-sync hooks install    # 安装 hooks
claude-code-sync hooks uninstall  # 卸载 hooks
claude-code-sync hooks show       # 查看状态

# 单独管理包装脚本
claude-code-sync wrapper install    # 创建 claude-sync
claude-code-sync wrapper uninstall  # 删除 claude-sync
claude-code-sync wrapper show       # 查看状态
```

### Hooks 说明

| Hook | 触发时机 | 功能 |
|------|----------|------|
| `SessionStart` | Claude Code 启动时 | 拉取最新历史（IDE 支持） |
| `Stop` | 每轮对话完成后 | 推送对话历史 |
| `UserPromptSubmit` | 每次发送消息时 | 检测新项目并拉取远程历史 |

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

除了对话历史，`claude-code-sync` 还支持同步 Claude Code 配置文件，让你在多个设备间保持一致的使用体验。

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
claude-code-sync config-sync push

# 查看远程所有设备配置
claude-code-sync config-sync list

# 应用其他设备的配置
claude-code-sync config-sync apply MacBook-Pro

# 查看配置同步状态
claude-code-sync config-sync status
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
claude-code-sync config-sync list
# 输出: MacBook-Pro, Windows-PC

# 应用 Mac 配置
claude-code-sync config-sync apply MacBook-Pro
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

## 常用命令示例

### 基本操作

| 命令 | 说明 |
|------|------|
| `claude-code-sync sync` | 双向同步 |
| `claude-code-sync pull` | 拉取远程更新 |
| `claude-code-sync push` | 推送本地更新 |
| `claude-code-sync status` | 查看同步状态 |
| `claude-code-sync update` | 检查更新 |
| `claude-code-sync automate` | 配置自动同步 |
| `claude-code-sync config-sync push` | 推送配置到远程 |
| `claude-code-sync config-sync list` | 列出远程设备配置 |
| `claude-code-sync config-sync apply <device>` | 应用其他设备配置 |
| `claude-code-sync config-sync status` | 查看配置同步状态 |
| `claude-code-sync hooks show` | 查看 hooks 状态 |
| `claude-code-sync wrapper show` | 查看包装脚本状态 |

### 配置管理

```bash
# 查看当前配置
claude-code-sync config --show

# 只同步最近 30 天的对话
claude-code-sync config --exclude-older-than 30

# 排除特定项目
claude-code-sync config --exclude-projects "*test*,*temp*"

# 只同步特定项目
claude-code-sync config --include-projects "*work*,*important*"
```

### 状态检查

```bash
# 基本状态
claude-code-sync status

# 显示文件列表
claude-code-sync status --show-files

# 查看冲突
claude-code-sync status --show-conflicts
```

### 冲突报告

```bash
# 生成 Markdown 报告
claude-code-sync report --format markdown

# 生成 JSON 报告并保存
claude-code-sync report --format json --output conflicts.json
```

---

## 高级配置

### Git LFS（大文件）

```bash
# 启用 LFS
claude-code-sync config --enable-lfs true

# 自定义 LFS 模式
claude-code-sync config --enable-lfs true --lfs-patterns "*.jsonl,*.png"
```

### 自定义同步目录

```bash
# 更改存储子目录（默认 "projects"）
claude-code-sync config --sync-subdirectory "claude-conversations"
```

### 自动化备份

**macOS/Linux crontab：**
```bash
# 每天晚上 11 点同步
0 23 * * * ~/.local/bin/claude-code-sync sync
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
claude-code-sync init --config ~/.claude-code-sync-init.toml
```

### 命令别名

**Bash/Zsh：**
```bash
alias ccs='claude-code-sync'
alias ccs-sync='claude-code-sync sync'
```

**PowerShell：**
```powershell
Set-Alias ccs claude-code-sync
```

---

## 故障排查

### 问题 1：No matching local project found

**原因：** 本地没有该项目或路径解析失败

**解决：**
1. 在本地用 Claude Code 打开该项目
2. 确保 `use_project_name_only = true` 已配置
3. 重新执行 `claude-code-sync pull`

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
1. 查看冲突报告：`claude-code-sync report`
2. 选择需要保留的版本
3. 删除不需要的文件
4. 推送：`claude-code-sync push`

### 问题 4：更新失败

```bash
# 检查更新
claude-code-sync update --check-only

# 手动更新
claude-code-sync update
```

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

*最后更新: 2026-02-04*
