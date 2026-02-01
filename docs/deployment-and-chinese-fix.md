# Claude Code Sync 部署指南与中文项目名修复

本文档记录了 claude-code-sync 的完整部署过程、多设备同步配置，以及中文项目名匹配问题的修复方案。

## 目录

- [Windows 部署](#windows-部署)
- [创建 GitHub 私有仓库](#创建-github-私有仓库)
- [初始化与首次同步](#初始化与首次同步)
- [启用多设备模式](#启用多设备模式)
- [Mac 部署](#mac-部署)
- [中文项目名问题](#中文项目名问题)
- [日常使用](#日常使用)

---

## Windows 部署

### 1. 安装 Rust

```bash
# 使用 winget 安装
winget install Rustlang.Rustup

# 重启终端后验证
rustc --version
cargo --version
```

### 2. 克隆并构建项目

```bash
git clone https://github.com/perfectra1n/claude-code-sync
cd claude-code-sync
cargo build --release
cargo install --path .
```

### 3. 验证安装

```bash
claude-code-sync --help
```

---

## 创建 GitHub 私有仓库

建议创建一个私有仓库来存储对话历史（包含敏感信息）：

1. 访问 https://github.com/new
2. 仓库名称：`claude-code-history`
3. 选择 **Private**
4. 点击 **Create repository**

---

## 初始化与首次同步

### 1. 初始化

```bash
claude-code-sync init \
  --local ~/claude-history-backup \
  --remote https://github.com/YOUR_USERNAME/claude-code-history.git
```

### 2. 设置默认分支为 main（可选）

如果你的 Git 默认使用 master 分支：

```bash
# 设置全局默认分支
git config --global init.defaultBranch main

# 重命名并同步
cd ~/claude-history-backup
git branch -m master main
git push -u origin main --force
git push origin --delete master
```

### 3. 首次推送

```bash
claude-code-sync push -m "Initial sync"
```

---

## 启用多设备模式

为了在不同设备间同步相同项目的对话历史（即使路径不同），需要启用 `use_project_name_only` 模式。

### 配置文件位置

| 平台 | 路径 |
|------|------|
| Windows | `%APPDATA%\claude-code-sync\config.toml` |
| macOS | `~/Library/Application Support/claude-code-sync/config.toml` |
| Linux | `~/.config/claude-code-sync/config.toml` |

### 配置内容

```toml
include_patterns = []
exclude_patterns = []
max_file_size_bytes = 10485760
exclude_attachments = false
enable_lfs = false
lfs_patterns = ["*.jsonl"]
scm_backend = "git"
sync_subdirectory = "projects"
use_project_name_only = true
```

### 验证配置

```bash
claude-code-sync config --show
```

确认显示 `Use project name only: Yes (multi-device mode)`

### 清理旧格式目录

启用多设备模式后重新同步，会产生新格式的目录（仅项目名）。旧格式目录（完整路径编码）可以删除：

```bash
cd ~/claude-history-backup/projects
rm -rf C--* c--*
git add -A && git commit -m "Clean up old full-path format folders"
git push
```

---

## Mac 部署

### 1. 安装 Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

### 2. 克隆并安装

```bash
git clone https://github.com/perfectra1n/claude-code-sync
cd claude-code-sync
cargo install --path .
```

### 3. 从远程克隆对话历史

```bash
claude-code-sync init \
  --local ~/claude-history-backup \
  --remote https://github.com/YOUR_USERNAME/claude-code-history.git \
  --clone
```

### 4. 配置多设备模式

```bash
mkdir -p ~/Library/Application\ Support/claude-code-sync

cat > ~/Library/Application\ Support/claude-code-sync/config.toml << 'EOF'
include_patterns = []
exclude_patterns = []
max_file_size_bytes = 10485760
exclude_attachments = false
enable_lfs = false
lfs_patterns = ["*.jsonl"]
scm_backend = "git"
sync_subdirectory = "projects"
use_project_name_only = true
EOF
```

### 5. 拉取对话历史

```bash
claude-code-sync pull
```

### GitHub 认证

如果遇到认证问题，推荐使用 GitHub CLI：

```bash
brew install gh
gh auth login
```

---

## 中文项目名问题

### 问题描述

Claude Code 在存储对话历史时，会将路径中的非 ASCII 字符（如中文）转换为 `-`：

- 原始路径：`/Users/mini/Documents/Projects/安装环境`
- 存储目录：`-Users-mini-Documents-Projects-----`（4个中文字符变成4个 `-`）

这导致在 `use_project_name_only` 模式下：

1. **Push 时**：从 JSONL 文件的 `cwd` 字段正确提取出 `安装环境`
2. **远程仓库**：以 `安装环境` 为目录名存储
3. **Pull 时**：尝试匹配本地目录，但 `extract_project_name("-Users-mini-Documents-Projects-----")` 返回 `Projects`
4. **匹配失败**：`Projects` ≠ `安装环境`

#### 深层问题

经过调试发现，中文项目名匹配失败的根本原因有**两个**：

**问题 1：跨平台路径解析**

- Windows 推送的 JSONL 文件包含 Windows 路径：`C:\Users\OSEN\Downloads\GitHub\安装环境`
- Mac/Linux 上使用 `std::path::Path::file_name()` 提取项目名时，无法识别 Windows 的 `\` 分隔符
- 结果：整个 `C:\Users\...\安装环境` 被当作一个文件名，无法提取出 `安装环境`

**问题 2：JSONL 文件扫描逻辑缺陷**

- 目录中可能有多个 JSONL 文件（对话文件、文件历史快照等）
- 如果第一个扫描到的文件是快照文件（没有 `cwd` 字段），原代码会直接 `break`
- 结果：跳过其他包含有效 `cwd` 的文件，匹配失败

### 修复方案

需要修改 **2 个文件** 来彻底解决问题：

#### 修复 1：`src/parser.rs` - 支持跨平台路径解析

修改 `project_name()` 函数，同时支持 Unix 和 Windows 路径分隔符：

```rust
/// Get the project name from the first entry's `cwd` path
///
/// This function handles both Unix and Windows paths to support
/// cross-platform sync (e.g., pulling Windows paths on Mac/Linux).
pub fn project_name(&self) -> Option<&str> {
    self.entries
        .iter()
        .find_map(|e| e.cwd.as_ref())
        .and_then(|cwd| {
            // Split by both / and \ to handle cross-platform paths
            // Take the last non-empty component
            cwd.split(&['/', '\\'])
                .filter(|s| !s.is_empty())
                .last()
        })
}
```

#### 修复 2：`src/sync/discovery.rs` - 改进 JSONL 扫描逻辑

修改 `find_local_project_by_name()` 函数的第二遍扫描逻辑：

```rust
// Second pass: read JSONL files to get real project name from cwd field
// This handles non-ASCII project names (e.g., Chinese) that get encoded as dashes
for entry in &entries {
    let dir_path = entry.path();

    // Try to find a .jsonl file with a valid project name in this directory
    if let Ok(files) = std::fs::read_dir(&dir_path) {
        for file_entry in files.filter_map(|f| f.ok()) {
            let file_path = file_entry.path();
            if file_path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                // Try to parse and get project name from cwd
                if let Ok(session) = crate::parser::ConversationSession::from_file(&file_path) {
                    if let Some(real_name) = session.project_name() {
                        // Found a valid project name, check if it matches
                        if real_name == project_name {
                            return Some(dir_path);
                        } else {
                            // Doesn't match, skip rest of this directory
                            break;
                        }
                    }
                    // If project_name() is None, continue to try next file
                }
            }
        }
    }
}
```

**关键改动**：
- ✅ 当 `project_name()` 返回 `None` 时，**继续尝试下一个 JSONL 文件**
- ✅ 只有在找到有效项目名但不匹配时，才跳过该目录

### 应用修复

修改代码后重新构建：

```bash
cd claude-code-sync
cargo build --release
cargo install --path . --force
```

### 验证修复

测试同步功能：

```bash
# 查看同步前的状态
claude-code-sync status

# 执行 pull
claude-code-sync pull

# 检查是否有"No matching local project found"警告
# 成功的话应该没有中文项目名的警告

# 查看同步后的状态（本地 sessions 数量应该增加）
claude-code-sync status
```

---

## 日常使用

### 常用命令

| 命令 | 说明 |
|------|------|
| `claude-code-sync sync` | 双向同步（拉取 + 推送），推荐日常使用 |
| `claude-code-sync pull` | 仅拉取远程变更 |
| `claude-code-sync push` | 仅推送本地变更 |
| `claude-code-sync status` | 查看同步状态 |
| `claude-code-sync config --show` | 查看当前配置 |

### 多机同步工作流

1. **开始工作前**：`claude-code-sync pull`
2. **结束工作后**：`claude-code-sync push`
3. **或一键同步**：`claude-code-sync sync`

### 注意事项

1. **项目文件夹名称一致**：确保不同设备上的项目文件夹名称相同（如都叫 `my-project`），这样对话历史才能正确匹配
2. **中文项目名**：需要应用上述修复才能正常工作
3. **私有仓库**：对话历史可能包含敏感信息，建议使用私有仓库

---

## 相关文件

- 配置文件：见上方各平台路径
- 状态文件：`%APPDATA%\claude-code-sync\state.json`（Windows）
- 同步仓库：`~/claude-history-backup`（默认位置）

---

## 修复历史

### 2026-02-01 - 完整修复跨平台路径问题

**修复内容：**
1. `src/parser.rs`: 支持跨平台路径解析（同时识别 `/` 和 `\`）
2. `src/sync/discovery.rs`: 改进 JSONL 扫描逻辑，支持多文件尝试

**修复效果：**
- ✅ 支持 Windows ↔ Mac/Linux 跨平台同步中文项目名
- ✅ 解决文件历史快照导致的扫描中断问题
- ✅ 完美匹配所有非 ASCII 字符项目名

---

*文档最后更新：2026-02-01*
