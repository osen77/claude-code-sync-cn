# 多设备同步简易指南

本指南帮助你在多台设备（如 Windows + Mac）之间同步 Claude Code 对话历史。

---

## 前置条件

✅ 已创建 GitHub 私有仓库（如 `claude-code-history`）
✅ 已在所有设备上安装 `claude-code-sync`
✅ 已配置 Git 认证（推荐使用 `gh auth login`）

---

## 一、首次设置

### 设备 A（主设备 - Windows）

#### 1. 安装工具

```bash
# 克隆并安装
git clone https://github.com/osen77/claude-code-sync-cn
cd claude-code-sync
cargo install --path .
```

#### 2. 初始化仓库

```bash
# 初始化并关联远程仓库
claude-code-sync init \
  --local C:\Users\YOUR_NAME\claude-history-backup \
  --remote https://github.com/YOUR_USERNAME/claude-code-history.git
```

#### 3. 配置多设备模式

编辑配置文件 `%APPDATA%\claude-code-sync\config.toml`：

```toml
include_patterns = []
exclude_patterns = []
max_file_size_bytes = 10485760
exclude_attachments = false
enable_lfs = false
lfs_patterns = ["*.jsonl"]
scm_backend = "git"
sync_subdirectory = "projects"
use_project_name_only = true  # ← 关键配置
```

或使用命令快速设置：

```bash
echo 'use_project_name_only = true' >> %APPDATA%\claude-code-sync\config.toml
```

#### 4. 首次推送

```bash
# 推送本地对话历史到远程
claude-code-sync push -m "Initial sync from Windows"

# 验证
claude-code-sync status
```

---

### 设备 B（Mac/Linux）

#### 1. 安装工具

```bash
# macOS
git clone https://github.com/osen77/claude-code-sync-cn
cd claude-code-sync
cargo install --path .

# 安装 GitHub CLI（推荐）
brew install gh
gh auth login
```

#### 2. 初始化并克隆远程仓库

```bash
# --clone 参数会自动拉取远程数据
claude-code-sync init \
  --local ~/claude-history-backup \
  --remote https://github.com/YOUR_USERNAME/claude-code-history.git \
  --clone
```

#### 3. 配置多设备模式

```bash
# 创建配置目录
mkdir -p ~/Library/Application\ Support/claude-code-sync

# 创建配置文件
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

#### 4. 验证配置

```bash
# 确认显示 "Use project name only: Yes"
claude-code-sync config --show

# 查看状态
claude-code-sync status
```

---

## 二、日常同步工作流

### 推荐流程：sync 命令（最简单）

```bash
# 开始工作前
claude-code-sync sync

# 结束工作后
claude-code-sync sync
```

`sync` 命令会自动：
1. 拉取远程更新（pull）
2. 合并本地变更
3. 推送到远程（push）

---

### 分步流程：pull + push

如果你想更精细地控制：

```bash
# 开始工作前 - 拉取最新对话
claude-code-sync pull

# 结束工作后 - 推送本地对话
claude-code-sync push -m "Update from Mac"
```

---

## 三、常用场景

### 场景 1：在 Windows 上工作后，切换到 Mac

**Windows 上**：
```bash
# 结束工作，推送更新
claude-code-sync push -m "Windows session"
```

**Mac 上**：
```bash
# 开始工作，拉取更新
claude-code-sync pull

# 现在可以看到 Windows 上的对话历史了
```

---

### 场景 2：两台设备同时修改了同一对话（冲突）

**自动处理**：
```bash
# Pull 时会自动检测冲突
claude-code-sync pull

# 如果有冲突，会保留两个版本：
# - session.jsonl（远程版本）
# - session-conflict-20260201-120000.jsonl（本地版本）

# 查看冲突报告
ls ~/claude-history-backup/.conflict-reports/
```

**手动解决**：
1. 打开两个冲突文件
2. 选择需要保留的版本
3. 删除不需要的文件
4. 推送解决后的结果

---

### 场景 3：定期备份（自动化）

**macOS/Linux**：
```bash
# 创建定时任务（crontab）
crontab -e

# 添加以下内容（每天晚上 10 点同步）
0 22 * * * /Users/YOUR_NAME/.cargo/bin/claude-code-sync sync
```

**Windows**：
使用任务计划程序创建定时任务。

---

## 四、重要注意事项

### ✅ 必须遵守

1. **项目名称一致**
   - 确保不同设备上的**项目文件夹名称相同**
   - ✅ 正确：Windows `C:\Projects\my-app`，Mac `/Users/mini/Projects/my-app`
   - ❌ 错误：Windows `C:\work\app1`，Mac `/Users/mini/code/myapp`

2. **同步时机**
   - **开始工作前**：执行 `pull` 或 `sync`
   - **结束工作后**：执行 `push` 或 `sync`
   - **切换设备时**：先在当前设备 push，再到新设备 pull

3. **中文项目名**
   - 确保已应用本仓库的跨平台路径修复
   - 验证方法：`claude-code-sync pull` 不应出现 "No matching local project found" 警告

### ⚠️ 常见陷阱

1. **忘记 push**
   - 在设备 A 工作后忘记 push，直接在设备 B 工作
   - 结果：设备 B 看不到最新对话
   - 解决：回到设备 A，执行 `claude-code-sync push`

2. **配置不一致**
   - 设备 A 启用了 `use_project_name_only`，设备 B 没启用
   - 结果：无法匹配项目
   - 解决：确保所有设备配置文件一致

3. **网络问题**
   - 推送/拉取失败
   - 解决：检查 Git 认证状态 `gh auth status`

---

## 五、快速命令参考

| 命令 | 说明 | 使用场景 |
|------|------|----------|
| `claude-code-sync sync` | 双向同步（拉取+推送） | 日常推荐 |
| `claude-code-sync pull` | 仅拉取远程更新 | 开始工作前 |
| `claude-code-sync push` | 仅推送本地更新 | 结束工作后 |
| `claude-code-sync status` | 查看同步状态 | 检查 sessions 数量 |
| `claude-code-sync config --show` | 查看当前配置 | 验证配置是否正确 |

---

## 六、验证同步是否成功

### 检查本地 sessions 数量

```bash
# 推送前
claude-code-sync status
# Local sessions: 50

# 推送
claude-code-sync push

# 在另一台设备上拉取
claude-code-sync pull

# 拉取后
claude-code-sync status
# Local sessions: 50（应该一致）
```

### 检查远程仓库

访问 GitHub 仓库：
```
https://github.com/YOUR_USERNAME/claude-code-history
```

查看 `projects/` 目录，应该能看到所有项目文件夹（按项目名命名）。

---

## 七、故障排查

### 问题 1：No matching local project found

**症状**：
```
[WARN] No matching local project found for '项目名'
```

**原因**：
- 本地没有该项目的对话历史
- 或跨平台路径解析失败（旧版本）

**解决**：
1. 在本地用 Claude Code 打开该项目，创建一个新对话
2. 确保已安装本仓库的修复版本
3. 重新执行 `claude-code-sync pull`

---

### 问题 2：Authentication failed

**症状**：
```
fatal: Authentication failed
```

**解决**：
```bash
# 使用 GitHub CLI 重新认证
gh auth login

# 或配置 SSH key
ssh-keygen -t ed25519 -C "your_email@example.com"
cat ~/.ssh/id_ed25519.pub  # 添加到 GitHub
```

---

### 问题 3：Merge conflicts

**症状**：
```
✓ Merged 0 sessions (3 conflicts)
```

**解决**：
1. 查看冲突报告：`cat ~/claude-history-backup/.conflict-reports/*.md`
2. 手动处理冲突文件
3. 删除不需要的版本
4. 推送解决结果

---

## 八、最佳实践

### 1. 定期同步

```bash
# 每天开始工作前
claude-code-sync sync

# 每次完成重要对话后
claude-code-sync push -m "Important conversation about X"
```

### 2. 备份重要对话

```bash
# 手动备份到特定分支
cd ~/claude-history-backup
git checkout -b backup-2026-02-01
git push origin backup-2026-02-01
```

### 3. 定期清理

```bash
# 查看仓库大小
du -sh ~/claude-history-backup

# 如果太大，考虑清理旧对话或启用 Git LFS
```

---

## 九、配置文件位置速查

| 平台 | 配置文件 | 本地仓库（默认） |
|------|---------|----------------|
| Windows | `%APPDATA%\claude-code-sync\config.toml` | `%USERPROFILE%\claude-history-backup` |
| macOS | `~/Library/Application Support/claude-code-sync/config.toml` | `~/claude-history-backup` |
| Linux | `~/.config/claude-code-sync/config.toml` | `~/claude-history-backup` |

---

## 十、进阶技巧

### 使用别名简化命令

**Bash/Zsh**：
```bash
# 添加到 ~/.bashrc 或 ~/.zshrc
alias ccs='claude-code-sync'
alias ccs-sync='claude-code-sync sync'
alias ccs-pull='claude-code-sync pull'
alias ccs-push='claude-code-sync push'
alias ccs-status='claude-code-sync status'

# 使用
ccs-sync  # 替代 claude-code-sync sync
```

**PowerShell**：
```powershell
# 添加到 PowerShell profile
Set-Alias ccs claude-code-sync
function ccs-sync { claude-code-sync sync }

# 使用
ccs-sync
```

---

## 需要帮助？

- **文档**: 查看 `docs/deployment-and-chinese-fix.md`
- **问题追踪**: https://github.com/osen77/claude-code-sync-cn/issues
- **上游项目**: https://github.com/perfectra1n/claude-code-sync

---

*最后更新: 2026-02-01*
