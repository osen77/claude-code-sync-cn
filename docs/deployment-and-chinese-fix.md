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

### 修复方案

修改 `src/sync/discovery.rs` 中的 `find_local_project_by_name()` 函数，增加基于 JSONL 内部 `cwd` 字段的匹配逻辑：

```rust
/// Find a local Claude project directory that matches the given project name.
///
/// First tries to match by extracting project name from encoded directory name.
/// If that fails (e.g., for non-ASCII project names like Chinese characters),
/// falls back to reading a JSONL file from each directory and extracting the
/// project name from the `cwd` field.
pub fn find_local_project_by_name(claude_projects_dir: &Path, project_name: &str) -> Option<PathBuf> {
    let entries: Vec<_> = std::fs::read_dir(claude_projects_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();

    // First pass: try matching by encoded directory name
    let matches: Vec<PathBuf> = entries
        .iter()
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|name| extract_project_name(name) == project_name)
                .unwrap_or(false)
        })
        .map(|e| e.path())
        .collect();

    // Return only if exactly one match to avoid ambiguity
    if matches.len() == 1 {
        return Some(matches.into_iter().next().unwrap());
    }

    // Second pass: read JSONL files to get real project name from cwd field
    // This handles non-ASCII project names (e.g., Chinese) that get encoded as dashes
    for entry in &entries {
        let dir_path = entry.path();

        // Find first .jsonl file in the directory
        if let Ok(files) = std::fs::read_dir(&dir_path) {
            for file_entry in files.filter_map(|f| f.ok()) {
                let file_path = file_entry.path();
                if file_path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                    // Try to parse and get project name from cwd
                    if let Ok(session) = crate::parser::ConversationSession::from_file(&file_path) {
                        if let Some(real_name) = session.project_name() {
                            if real_name == project_name {
                                return Some(dir_path);
                            }
                        }
                    }
                    break; // Only need to check one file per directory
                }
            }
        }
    }

    None
}
```

### 应用修复

修改代码后重新构建：

```bash
cd claude-code-sync
cargo build --release
cargo install --path . --force
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

*文档更新日期：2026-02-01*
