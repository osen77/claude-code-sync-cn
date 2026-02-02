# Claude Code Sync 自动化同步指南

本指南介绍如何自动化 claude-code-sync 的同步流程，减少手动操作，确保对话历史实时备份。

---

## 方案概览

| 方案 | 适用场景 | 实时性 | 复杂度 | 推荐度 |
|------|---------|--------|--------|--------|
| **Claude Code Hooks - 方案 A**  | 简单自动化 | ⭐⭐⭐⭐ | ⭐⭐ | 推荐 |
| **Claude Code Hooks - 方案 B** | 精确控制新项目 | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ | 🔥 **强烈推荐** |
| **Claude Code Hooks - 方案 C** | 实时同步 | ⭐⭐⭐⭐⭐ | ⭐⭐ | 适合高频用户 |
| **系统定时任务** | 定期备份 | ⭐⭐⭐ | ⭐⭐ | 辅助方案 |
| **文件监控** | 实时同步 | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ | 高级用户 |

---

## 方案 1: Claude Code Hooks

Claude Code 支持配置 hooks(钩子),可以在特定事件发生时自动执行命令。这是最适合与 Claude Code 工作流集成的方案。

### 配置位置

Claude Code 的配置文件位置:
- **macOS**: `~/Library/Application Support/Claude/claude_desktop_config.json`
- **Linux**: `~/.config/Claude/claude_desktop_config.json`
- **Windows**: `%APPDATA%\Claude\claude_desktop_config.json`

### 可用的 Hook 事件

根据 Claude Code 的最新版本（v2.x+），支持以下核心事件:

| 事件名称 | 触发时机 | 典型用途 |
|---------|---------|----------|
| `SessionStart` | 会话启动或恢复时 | 拉取最新对话历史 |
| `SessionEnd` | 会话结束时 | 推送本地对话 |
| `Stop` | AI 完成整段响应后 | 中间同步检查点 |
| `UserPromptSubmit` | 用户提交提示词后 | 注入上下文 |
| `PreToolUse` | 执行工具前 | 权限检查 |
| `PostToolUse` | 工具执行后 | 自动格式化 |

---

### 方案 A: 简单方案 (SessionStart + SessionEnd)

**适合场景**: 一般用户，配置最简单

#### 配置示例

```json
{
  "hooks": {
    "SessionStart": "claude-code-sync pull 2>/dev/null &",
    "SessionEnd": "claude-code-sync push -m 'Auto-sync on session end' --exclude-attachments > /dev/null 2>&1 &"
  }
}
```

#### 行为说明

- **SessionStart**: 会话开始时拉取最新对话
- **SessionEnd**: 会话结束时推送本地对话
- **新项目处理**: 第一次会话 pull 会失败（被静默），第二次会话正常

#### 优缺点

**优点**:
- ✅ 配置极简，无需额外脚本
- ✅ 覆盖大部分使用场景
- ✅ 性能影响极小

**缺点**:
- ❌ 新项目第一次 pull 会失败（虽然被静默）
- ❌ 不够精确，可能有轻微延迟

---

### 方案 B: 精确方案 (SessionStart + Stop + SessionEnd) 🔥

**适合场景**: 多设备工作，需要精确控制新项目同步时机

#### 核心需求

对于**新项目**（本地还没有 Claude 对话历史的项目）:
1. 在第一次对话**之前**，`~/.claude/projects/` 下还没有项目目录
2. 用户发送第一个问题后，Claude 创建目录和 `.jsonl` 文件
3. **此时才执行 pull**，拉取远程可能存在的该项目历史
4. 这样可以避免无效的 pull，并且能正确匹配项目

#### 事件组合

| Hook 事件 | 触发时机 | 执行操作 | 目的 |
|-----------|---------|---------|------|
| `SessionStart` | 会话启动 | 检查项目目录是否存在，存在则 pull | 已有项目同步最新对话 |
| `Stop` | AI 响应完成 | 检查是否新项目首次响应，是则 pull | 新项目拉取远程历史 |
| `SessionEnd` | 会话结束 | 始终执行 push | 备份本地对话 |

#### 实现步骤

##### 第 1 步: 创建脚本目录

```bash
mkdir -p ~/scripts/claude-hooks
chmod +x ~/scripts/claude-hooks
```

##### 第 2 步: 创建 SessionStart 脚本

创建文件 `~/scripts/claude-hooks/claude-smart-pull.sh`:

```bash
#!/bin/bash
# SessionStart Hook: 如果项目已有对话历史，执行 pull

# 配置
LOG_FILE="$HOME/claude-hooks.log"
CLAUDE_PROJECTS_DIR="$HOME/.claude/projects"

# 提取项目名
PROJECT_NAME=$(basename "$PWD")

# 查找匹配的 Claude 项目目录
# 兼容两种模式:
# - use_project_name_only=true: 目录名是纯项目名 (如 "myproject")
# - use_project_name_only=false: 目录名是路径编码 (如 "-Users-mini-Documents-myproject")
CLAUDE_DIR=$(find "$CLAUDE_PROJECTS_DIR" -maxdepth 1 -type d \
    \( -name "$PROJECT_NAME" -o -name "*-$PROJECT_NAME" \) \
    2>/dev/null | head -n 1)

# 如果目录存在且包含 .jsonl 文件，执行 pull
if [ -n "$CLAUDE_DIR" ] && [ -d "$CLAUDE_DIR" ]; then
    if ls "$CLAUDE_DIR"/*.jsonl 1>/dev/null 2>&1; then
        echo "[$(date +'%Y-%m-%d %H:%M:%S')] [SessionStart] Pulling for project: $PROJECT_NAME" >> "$LOG_FILE"
        claude-code-sync pull >> "$LOG_FILE" 2>&1 &
    else
        echo "[$(date +'%Y-%m-%d %H:%M:%S')] [SessionStart] Skipping pull (no JSONL files): $PROJECT_NAME" >> "$LOG_FILE"
    fi
else
    echo "[$(date +'%Y-%m-%d %H:%M:%S')] [SessionStart] Skipping pull (new project): $PROJECT_NAME" >> "$LOG_FILE"
fi

exit 0
```

```bash
# 赋予执行权限
chmod +x ~/scripts/claude-hooks/claude-smart-pull.sh
```

##### 第 3 步: 创建 Stop 脚本

创建文件 `~/scripts/claude-hooks/claude-first-response-pull.sh`:

```bash
#!/bin/bash
# Stop Hook: 新项目首次响应后执行 pull

# 配置
LOG_FILE="$HOME/claude-hooks.log"
STATE_DIR="$HOME/.claude-code-sync/first-pull-done"
CLAUDE_PROJECTS_DIR="$HOME/.claude/projects"

# 提取项目名
PROJECT_NAME=$(basename "$PWD")
STATE_FILE="$STATE_DIR/$PROJECT_NAME"

# 如果已经执行过首次 pull，直接退出（避免重复）
if [ -f "$STATE_FILE" ]; then
    exit 0
fi

# 查找项目目录
CLAUDE_DIR=$(find "$CLAUDE_PROJECTS_DIR" -maxdepth 1 -type d \
    \( -name "$PROJECT_NAME" -o -name "*-$PROJECT_NAME" \) \
    2>/dev/null | head -n 1)

# 如果目录存在且有 .jsonl 文件，执行首次 pull
if [ -n "$CLAUDE_DIR" ] && [ -d "$CLAUDE_DIR" ]; then
    if ls "$CLAUDE_DIR"/*.jsonl 1>/dev/null 2>&1; then
        echo "[$(date +'%Y-%m-%d %H:%M:%S')] [Stop] First pull for new project: $PROJECT_NAME" >> "$LOG_FILE"

        # 执行 pull
        claude-code-sync pull >> "$LOG_FILE" 2>&1 &

        # 创建状态标记，避免重复 pull
        mkdir -p "$STATE_DIR"
        touch "$STATE_FILE"

        echo "[$(date +'%Y-%m-%d %H:%M:%S')] [Stop] Marked as pulled: $STATE_FILE" >> "$LOG_FILE"
    fi
fi

exit 0
```

```bash
# 赋予执行权限
chmod +x ~/scripts/claude-hooks/claude-first-response-pull.sh
```

##### 第 4 步: 配置 Claude Code Hooks

编辑 `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "hooks": {
    "SessionStart": "/Users/YOUR_USERNAME/scripts/claude-hooks/claude-smart-pull.sh",
    "Stop": "/Users/YOUR_USERNAME/scripts/claude-hooks/claude-first-response-pull.sh",
    "SessionEnd": "claude-code-sync push -m 'Auto-sync on session end' --exclude-attachments > /dev/null 2>&1 &"
  }
}
```

**注意**:
- 替换 `YOUR_USERNAME` 为实际用户名
- 可以使用绝对路径或 `~`（如果 Claude Code 支持）
- 如果 `claude-code-sync` 不在 PATH 中，使用完整路径: `/Users/YOUR_USERNAME/.cargo/bin/claude-code-sync`

##### 第 5 步: 测试脚本

```bash
# 手动测试 SessionStart 脚本
cd ~/your-test-project
~/scripts/claude-hooks/claude-smart-pull.sh

# 查看日志
tail -f ~/claude-hooks.log
```

##### 第 6 步: 验证 Hook 配置

1. 重启 Claude Code（如需要）
2. 打开一个项目并发送对话
3. 检查日志: `tail -f ~/claude-hooks.log`
4. 检查状态目录: `ls ~/.claude-code-sync/first-pull-done/`

#### 测试场景

##### 场景 1: 新项目首次对话

```bash
# 步骤
cd ~/new-test-project
# 启动 Claude Code 并发送第一个问题

# 预期行为:
# 1. SessionStart: 日志显示 "Skipping pull (new project)"
# 2. AI 响应后 Stop: 日志显示 "First pull for new project"
# 3. 状态文件被创建: ls ~/.claude-code-sync/first-pull-done/new-test-project
# 4. SessionEnd: 执行 push

# 验证命令
tail -20 ~/claude-hooks.log
ls -la ~/.claude-code-sync/first-pull-done/
claude-code-sync status
```

##### 场景 2: 已有项目

```bash
# 步骤
cd ~/existing-project  # 已有对话历史的项目
# 启动 Claude Code

# 预期行为:
# 1. SessionStart: 日志显示 "Pulling for project"
# 2. Stop: 检测到状态文件，静默退出（日志无新增）
# 3. SessionEnd: 执行 push

# 验证命令
grep "existing-project" ~/claude-hooks.log | tail -10
```

##### 场景 3: 跨设备同步（新项目）

```bash
# === 设备 A (已有对话) ===
cd ~/my-shared-project
# 发送对话...
# SessionEnd 会自动 push

# === 设备 B (首次打开) ===
cd ~/my-shared-project  # 本地还没有 Claude 对话历史
# 启动 Claude Code，发送第一个问题

# 预期行为（设备 B）:
# 1. SessionStart: 跳过 pull（本地无历史）
# 2. AI 响应后 Stop: 执行 pull，拉取设备 A 的对话历史 ✅
# 3. Claude Code UI 显示设备 A 的历史对话

# 验证
ls ~/.claude/projects/*my-shared-project*/*.jsonl
# 应该能看到从设备 A 拉取的对话文件
```

#### 状态管理和清理

方案 B 使用状态文件避免重复操作:

**状态文件位置**: `~/.claude-code-sync/first-pull-done/<project-name>`

**查看状态**:
```bash
ls -la ~/.claude-code-sync/first-pull-done/
```

**清理特定项目状态** (强制下次 Stop 重新 pull):
```bash
rm ~/.claude-code-sync/first-pull-done/my-project
```

**完全重置**:
```bash
rm -rf ~/.claude-code-sync/first-pull-done/*
```

**自动清理（可选）**:
添加到 crontab，每月清理超过 30 天未访问的状态:
```bash
crontab -e
# 添加:
0 0 1 * * find ~/.claude-code-sync/first-pull-done/ -type f -atime +30 -delete
```

#### Windows PowerShell 版本

##### claude-smart-pull.ps1 (SessionStart)

```powershell
# SessionStart Hook for Windows
$ProjectName = Split-Path -Leaf (Get-Location)
$LogFile = "$env:USERPROFILE\claude-hooks.log"
$ClaudeProjectsDir = "$env:USERPROFILE\.claude\projects"

# 查找匹配的项目目录
$ClaudeDir = Get-ChildItem $ClaudeProjectsDir -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -eq $ProjectName -or $_.Name -like "*-$ProjectName" } |
    Select-Object -First 1

if ($ClaudeDir -and (Test-Path "$($ClaudeDir.FullName)\*.jsonl")) {
    Add-Content -Path $LogFile -Value "[$((Get-Date).ToString('yyyy-MM-dd HH:mm:ss'))] [SessionStart] Pulling: $ProjectName"
    Start-Process -WindowStyle Hidden -FilePath "claude-code-sync" -ArgumentList "pull"
} else {
    Add-Content -Path $LogFile -Value "[$((Get-Date).ToString('yyyy-MM-dd HH:mm:ss'))] [SessionStart] Skipping pull (new project): $ProjectName"
}
```

##### claude-first-response-pull.ps1 (Stop)

```powershell
# Stop Hook for Windows
$ProjectName = Split-Path -Leaf (Get-Location)
$LogFile = "$env:USERPROFILE\claude-hooks.log"
$StateDir = "$env:USERPROFILE\.claude-code-sync\first-pull-done"
$StateFile = "$StateDir\$ProjectName"
$ClaudeProjectsDir = "$env:USERPROFILE\.claude\projects"

# 如果已经执行过，直接退出
if (Test-Path $StateFile) {
    exit 0
}

# 查找项目目录
$ClaudeDir = Get-ChildItem $ClaudeProjectsDir -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -eq $ProjectName -or $_.Name -like "*-$ProjectName" } |
    Select-Object -First 1

if ($ClaudeDir -and (Test-Path "$($ClaudeDir.FullName)\*.jsonl")) {
    Add-Content -Path $LogFile -Value "[$((Get-Date).ToString('yyyy-MM-dd HH:mm:ss'))] [Stop] First pull: $ProjectName"
    Start-Process -WindowStyle Hidden -FilePath "claude-code-sync" -ArgumentList "pull"

    # 创建状态标记
    if (-not (Test-Path $StateDir)) {
        New-Item -Path $StateDir -ItemType Directory -Force | Out-Null
    }
    New-Item -Path $StateFile -ItemType File -Force | Out-Null

    Add-Content -Path $LogFile -Value "[$((Get-Date).ToString('yyyy-MM-dd HH:mm:ss'))] [Stop] Marked as pulled"
}
```

**Windows 配置** (`%APPDATA%\Claude\claude_desktop_config.json`):
```json
{
  "hooks": {
    "SessionStart": "C:\\Users\\YOUR_USERNAME\\scripts\\claude-hooks\\claude-smart-pull.ps1",
    "Stop": "C:\\Users\\YOUR_USERNAME\\scripts\\claude-hooks\\claude-first-response-pull.ps1",
    "SessionEnd": "powershell -WindowStyle Hidden -Command \"claude-code-sync push -m 'Auto-sync' --exclude-attachments\""
  }
}
```

#### 故障排查

##### 问题 1: Hook 没有执行

**症状**: 日志文件没有新增记录

**排查步骤**:
```bash
# 1. 检查脚本权限
ls -l ~/scripts/claude-hooks/*.sh
# 应该显示 -rwxr-xr-x

# 2. 手动运行脚本测试
cd ~/test-project
~/scripts/claude-hooks/claude-smart-pull.sh
tail -5 ~/claude-hooks.log

# 3. 检查 Claude Code 配置
cat ~/Library/Application\ Support/Claude/claude_desktop_config.json

# 4. 检查脚本路径是否正确
which claude-code-sync
# 如果找不到，需要在脚本中使用完整路径
```

**解决方法**:
- 确保脚本有执行权限: `chmod +x ~/scripts/claude-hooks/*.sh`
- 使用绝对路径: `/Users/YOUR_NAME/.cargo/bin/claude-code-sync`
- 检查 JSON 格式是否正确（注意逗号、引号）

##### 问题 2: Pull 失败（远程仓库未配置）

**症状**: 日志显示 "Sync not initialized" 或 Git 错误

**解决方法**:
```bash
# 检查 sync 状态
claude-code-sync status

# 如果未初始化，先初始化
claude-code-sync init --repo ~/claude-history-backup --remote git@github.com:user/repo.git

# 测试 pull
claude-code-sync pull
```

##### 问题 3: 项目目录找不到

**症状**: 日志显示 "Skipping pull (new project)" 但实际有对话历史

**排查**:
```bash
# 检查项目目录结构
ls -la ~/.claude/projects/

# 手动查找项目
PROJECT_NAME=$(basename "$PWD")
find ~/.claude/projects -type d -name "*$PROJECT_NAME*"

# 检查项目名是否正确
echo "Current project: $PROJECT_NAME"
```

**解决方法**:
- 确认项目名匹配逻辑正确
- 检查是否启用了 `use_project_name_only` (查看配置: `claude-code-sync config --show`)
- 手动调整脚本匹配规则

##### 问题 4: 状态文件未创建

**症状**: 每次 Stop 都执行 pull

**排查**:
```bash
# 检查状态目录权限
ls -ld ~/.claude-code-sync/first-pull-done/
# 如果不存在，手动创建
mkdir -p ~/.claude-code-sync/first-pull-done

# 检查脚本是否有写权限
touch ~/.claude-code-sync/first-pull-done/test
rm ~/.claude-code-sync/first-pull-done/test
```

##### 问题 5: 日志文件过大

**症状**: `~/claude-hooks.log` 占用空间过大

**解决方法**:
```bash
# 清空日志
echo "" > ~/claude-hooks.log

# 或设置日志轮转（添加到 cron）
# 每月清理一次
0 0 1 * * mv ~/claude-hooks.log ~/claude-hooks.log.old && touch ~/claude-hooks.log
```

#### 优缺点

**优点**:
- ✅ 精确控制新项目首次 pull 时机
- ✅ 避免无效操作，提升效率
- ✅ 完全符合"新项目等第一个对话后才拉取"需求
- ✅ 状态管理，避免重复 pull
- ✅ 兼容 `use_project_name_only` 两种模式

**缺点**:
- ❌ 配置稍复杂，需要维护脚本和状态文件
- ❌ 需要一定的 Shell 脚本知识进行排查

---

### 方案 C: 实时方案 (Stop + SessionEnd)

**适合场景**: 需要极致实时性，每次 AI 响应后都同步

#### 配置示例

```json
{
  "hooks": {
    "Stop": "claude-code-sync sync 2>/dev/null &",
    "SessionEnd": "claude-code-sync push -m 'Auto-sync on session end' --exclude-attachments > /dev/null 2>&1 &"
  }
}
```

#### 行为说明

- **Stop**: 每次 AI 响应完成后执行双向同步（pull + push）
- **SessionEnd**: 会话结束时再次 push（保险）

#### 优缺点

**优点**:
- ✅ 实时性最强，每次响应后立即同步
- ✅ 数据丢失风险最小

**缺点**:
- ❌ 频繁触发同步，可能影响性能
- ❌ 网络开销较大
- ❌ 适合网络条件好的场景

---

## 方案对比与选择指南

| 特性 | 方案 A (简单) | 方案 B (精确) | 方案 C (实时) |
|------|-------------|-------------|-------------|
| **新项目首次行为** | pull 失败(静默) | 首次响应后 pull ✅ | 每次响应都 sync |
| **配置复杂度** | ⭐ | ⭐⭐⭐ | ⭐⭐ |
| **适用场景** | 一般用户 | 多设备/严格同步 | 高实时性需求 |
| **性能影响** | 极小 | 极小 | 中等(频繁同步) |
| **状态管理** | 无 | 有（状态文件） | 无 |
| **跨平台支持** | ✅ | ✅ (Bash + PowerShell) | ✅ |
| **推荐度** | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ |

### 选择建议

| 场景 | 推荐方案 | 原因 |
|------|---------|------|
| 个人单机使用 | **方案 A** | 简单够用，无需复杂配置 |
| **多设备频繁切换** | **方案 B** 🔥 | 精确控制，避免无效 pull |
| 新项目同步需求强 | **方案 B** 🔥 | 首次对话后立即拉取远程历史 |
| 团队协作 | **方案 C** | 实时同步，减少冲突 |
| 网络不稳定 | **方案 A** | 错误静默，不影响使用 |
| 极致实时性 | **方案 C** | 每次响应都同步 |

### 推荐组合

最佳实践是组合使用多种方案:

```
方案 B (Claude Code Hooks)
        +
系统定时任务 (每 4 小时兜底同步)
        +
Shell 别名 (手动快速操作)
```

---

## 方案 2: 系统定时任务

适合定期备份场景，作为 Hooks 方案的补充。

### macOS - launchd (推荐)

launchd 是 macOS 的推荐定时任务系统,比 cron 更可靠。

#### 创建 plist 文件

```bash
nano ~/Library/LaunchAgents/com.claude-code-sync.plist
```

**配置内容**:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.claude-code-sync</string>

    <key>ProgramArguments</key>
    <array>
        <string>/Users/YOUR_NAME/.cargo/bin/claude-code-sync</string>
        <string>sync</string>
    </array>

    <key>StandardOutPath</key>
    <string>/Users/YOUR_NAME/claude-sync.log</string>

    <key>StandardErrorPath</key>
    <string>/Users/YOUR_NAME/claude-sync-error.log</string>

    <!-- 每 4 小时运行一次 -->
    <key>StartInterval</key>
    <integer>14400</integer>

    <!-- 启动时运行一次 -->
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
```

#### 加载和管理

```bash
# 加载任务
launchctl load ~/Library/LaunchAgents/com.claude-code-sync.plist

# 卸载任务
launchctl unload ~/Library/LaunchAgents/com.claude-code-sync.plist

# 查看任务状态
launchctl list | grep claude-code-sync

# 手动触发(测试)
launchctl start com.claude-code-sync

# 查看日志
tail -f ~/claude-sync.log
```

### Linux - cron

```bash
# 编辑 crontab
crontab -e

# 添加任务示例
# 每天晚上 10 点同步
0 22 * * * /home/YOUR_NAME/.cargo/bin/claude-code-sync sync >> ~/claude-sync.log 2>&1

# 每 4 小时同步一次
0 */4 * * * /home/YOUR_NAME/.cargo/bin/claude-code-sync sync

# 工作日每 2 小时同步(周一到周五,9-18 点)
0 9-18/2 * * 1-5 /home/YOUR_NAME/.cargo/bin/claude-code-sync sync
```

**Cron 时间格式说明**:
```
* * * * * 命令
│ │ │ │ │
│ │ │ │ └─── 星期 (0-7, 0 和 7 都表示周日)
│ │ │ └───── 月份 (1-12)
│ │ └─────── 日期 (1-31)
│ └───────── 小时 (0-23)
└─────────── 分钟 (0-59)
```

### Windows - 任务计划程序

#### 使用 PowerShell 创建任务

```powershell
# 创建每 4 小时运行一次的任务
$action = New-ScheduledTaskAction -Execute "C:\Users\YOUR_NAME\.cargo\bin\claude-code-sync.exe" -Argument "sync"
$trigger = New-ScheduledTaskTrigger -Once -At (Get-Date) -RepetitionInterval (New-TimeSpan -Hours 4) -RepetitionDuration ([TimeSpan]::MaxValue)
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
Register-ScheduledTask -TaskName "ClaudeCodeSync" -Action $action -Trigger $trigger -Settings $settings -Description "Auto sync Claude Code history"

# 查看任务
Get-ScheduledTask -TaskName "ClaudeCodeSync"

# 手动触发(测试)
Start-ScheduledTask -TaskName "ClaudeCodeSync"

# 删除任务
Unregister-ScheduledTask -TaskName "ClaudeCodeSync" -Confirm:$false
```

---

## 最佳实践建议

### 1. 推荐组合配置

```
Claude Code Hooks - 方案 B (会话自动同步)
        +
launchd/cron 定时任务 (每 4 小时兜底同步)
        +
Shell 别名 (手动快速操作)
```

这个组合可以确保:
- ✅ 日常工作自动备份
- ✅ 即使忘记关闭会话也能定时同步
- ✅ 需要时可以手动强制同步

### 2. 避免过于频繁的同步

- ❌ 不推荐: 每分钟同步一次
- ✅ 推荐:
  - Session 结束时同步
  - 每 2-4 小时定时同步
  - 工作开始/结束手动同步

### 3. 使用 `--exclude-attachments`

如果同步频繁,建议排除大文件附件,只同步 JSONL:

```bash
claude-code-sync sync --exclude-attachments
```

或在配置中永久设置:
```bash
claude-code-sync config --exclude-attachments true
```

### 4. 监控同步状态

定期检查同步日志:

```bash
# 查看最近的同步操作
claude-code-sync history list

# 查看同步状态
claude-code-sync status

# 查看 Hook 日志
tail -f ~/claude-hooks.log
```

### 5. 配置异常处理

在自动化脚本中加入错误处理和通知:

```bash
#!/bin/bash

if ! claude-code-sync sync; then
    # macOS 通知
    osascript -e 'display notification "Sync failed!" with title "Claude Code Sync" sound name "Basso"'

    # 或发送邮件通知
    echo "Claude sync failed at $(date)" | mail -s "Sync Failed" you@example.com
fi
```

---

## Shell 别名/函数

简化手动操作的轻量方案。

### Bash/Zsh

```bash
# 添加到 ~/.bashrc 或 ~/.zshrc

# 基础别名
alias ccs='claude-code-sync'
alias ccs-sync='claude-code-sync sync'
alias ccs-push='claude-code-sync push -m "Manual push"'
alias ccs-pull='claude-code-sync pull'
alias ccs-status='claude-code-sync status'
alias ccs-history='claude-code-sync history list'

# 快速查看日志
alias ccs-log='tail -f ~/claude-hooks.log'

# 智能函数 - 工作开始和结束
ccs-start() {
    echo "Pulling latest Claude history..."
    claude-code-sync pull
}

ccs-end() {
    echo "Pushing Claude history..."
    claude-code-sync push -m "Work session $(date +%Y-%m-%d)"
}

# 自动同步并显示通知(macOS)
ccs-auto() {
    if claude-code-sync sync; then
        osascript -e 'display notification "Sync successful" with title "Claude Code Sync"'
    else
        osascript -e 'display notification "Sync failed!" with title "Claude Code Sync"'
    fi
}
```

**使用**:

```bash
# 开始工作
ccs-start

# 结束工作
ccs-end

# 快速同步
ccs-sync

# 查看状态
ccs-status
```

---

## 完整配置示例 (macOS)

以下是方案 B + 定时任务 + Shell 别名的完整配置:

### 第 1 步: 配置 Claude Code Hooks

编辑 `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "hooks": {
    "SessionStart": "/Users/YOUR_USERNAME/scripts/claude-hooks/claude-smart-pull.sh",
    "Stop": "/Users/YOUR_USERNAME/scripts/claude-hooks/claude-first-response-pull.sh",
    "SessionEnd": "claude-code-sync push -m 'Auto-sync on session end' --exclude-attachments > /dev/null 2>&1 &"
  }
}
```

### 第 2 步: 配置 launchd 定时同步

创建 `~/Library/LaunchAgents/com.claude-code-sync.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.claude-code-sync</string>
    <key>ProgramArguments</key>
    <array>
        <string>/Users/YOUR_NAME/.cargo/bin/claude-code-sync</string>
        <string>sync</string>
    </array>
    <key>StandardOutPath</key>
    <string>/Users/YOUR_NAME/claude-sync.log</string>
    <key>StandardErrorPath</key>
    <string>/Users/YOUR_NAME/claude-sync-error.log</string>
    <key>StartInterval</key>
    <integer>14400</integer>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
```

```bash
launchctl load ~/Library/LaunchAgents/com.claude-code-sync.plist
```

### 第 3 步: 配置 Shell 别名

添加到 `~/.zshrc`:

```bash
# Claude Code Sync 别名
alias ccs='claude-code-sync'
alias ccs-sync='claude-code-sync sync'
alias ccs-status='claude-code-sync status'
alias ccs-history='claude-code-sync history list'
alias ccs-log='tail -f ~/claude-hooks.log'
```

```bash
source ~/.zshrc
```

---

## 总结

### 推荐方案总结

| 用户类型 | 推荐配置 | 复杂度 |
|---------|---------|--------|
| **普通用户** | 方案 A (Hooks) | ⭐⭐ |
| **多设备用户** | 方案 B (Hooks) 🔥 | ⭐⭐⭐ |
| **团队协作** | 方案 C (Hooks) + 定时任务 | ⭐⭐⭐ |
| **高级用户** | 方案 B + 定时任务 + 别名 | ⭐⭐⭐⭐ |

### 关键特性对比

| 特性 | Hooks 方案 A | Hooks 方案 B 🔥 | 定时任务 |
|------|------------|----------------|---------|
| **自动化程度** | 高 | 最高 | 中 |
| **实时性** | 高 | 最高 | 低 |
| **新项目处理** | 失败(静默) | 精确 ✅ | 延迟 |
| **配置难度** | 低 | 中 | 低 |
| **适用场景** | 一般使用 | 多设备同步 | 定期备份 |

---

**下一步**:
- [多设备同步指南](multi-device-sync-guide.md)
- [项目文档主页](../CLAUDE.md)

---

*最后更新: 2026-02-01*
