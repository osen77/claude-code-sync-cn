# Claude Code Sync 项目指南

本文档为 claude-code-sync 项目的 AI 助手协作指南，包含架构说明、开发规范和重要注意事项。

## 项目概述

claude-code-sync 是一个 Rust CLI 工具，用于同步 Claude Code 对话历史到 Git/Mercurial 仓库，支持跨设备备份和同步。

- **语言**: Rust 2021 Edition
- **核心功能**: 对话历史同步、冲突解决、跨平台路径处理
- **支持平台**: Windows、macOS、Linux
- **版本控制**: Git (主要) / Mercurial (可选)

## 架构说明

### 模块分层

```
claude-code-sync/
├── src/
│   ├── main.rs              # CLI 入口
│   ├── lib.rs               # 库入口
│   │
│   ├── sync/                # 同步核心模块
│   │   ├── discovery.rs     # 🔑 项目发现和匹配逻辑
│   │   ├── pull.rs          # 拉取远程变更
│   │   ├── push.rs          # 推送本地变更
│   │   ├── init.rs          # 仓库初始化
│   │   ├── state.rs         # 同步状态管理
│   │   └── remote.rs        # 远程操作
│   │
│   ├── parser.rs            # 🔑 JSONL 文件解析
│   ├── scm/                 # 版本控制抽象层
│   │   ├── git.rs           # Git 实现
│   │   ├── hg.rs            # Mercurial 实现
│   │   └── lfs.rs           # Git LFS 支持
│   │
│   ├── merge.rs             # 对话合并逻辑
│   ├── conflict.rs          # 冲突检测
│   ├── interactive_conflict.rs  # 交互式冲突解决
│   │
│   ├── handlers/            # 命令处理器
│   │   ├── setup.rs         # 🔑 交互式配置向导
│   │   ├── update.rs        # 🔑 自动更新功能
│   │   ├── automate.rs      # 🔑 一键自动化配置
│   │   ├── hooks.rs         # Claude Code Hooks 管理
│   │   └── wrapper.rs       # 启动包装脚本
│   ├── history/             # 操作历史记录
│   ├── undo/                # 撤销操作
│   ├── filter.rs            # 同步过滤器
│   └── config.rs            # 配置管理
│
└── docs/
    └── user-guide.md        # 用户指南（安装、同步、命令示例）
```

### 关键数据流

1. **Push 流程**:
   ```
   ~/.claude/projects/ → discovery.rs (扫描)
   → parser.rs (解析 JSONL)
   → filter.rs (过滤)
   → push.rs (复制到 sync repo)
   → scm (提交推送)
   ```

2. **Pull 流程**:
   ```
   remote repo → pull.rs (拉取)
   → discovery.rs (匹配本地项目) ⚠️
   → merge.rs (合并)
   → 复制到 ~/.claude/projects/
   ```

## 核心功能说明

### 1. 项目名匹配 (`sync/discovery.rs`)

**关键函数**: `find_local_project_by_name()`

**匹配策略**:
- **第一遍**: 从目录名编码提取项目名（如 `-Users-mini-Documents-myproject` → `myproject`）
- **第二遍**: 从 JSONL 文件的 `cwd` 字段提取真实项目名（处理中文等非 ASCII 字符）

**重要**:
- 支持跨平台路径（Windows `\` 和 Unix `/`）
- 跳过没有 `cwd` 的文件（如快照文件），继续尝试其他 JSONL

### 2. 路径解析 (`parser.rs`)

**关键函数**: `ConversationSession::project_name()`

**实现**:
```rust
// ✅ 同时支持 Unix 和 Windows 路径分隔符
cwd.split(&['/', '\\'])
    .filter(|s| !s.is_empty())
    .last()
```

**用途**: 从 `cwd` 字段提取项目名，支持跨平台同步

### 3. 多设备模式

**配置**: `use_project_name_only = true`

**效果**:
- 仅使用项目名作为目录名（如 `myproject`）
- 不使用完整路径编码（如 `-Users-mini-Documents-myproject`）
- 支持不同设备上路径不同但项目名相同的场景

### 4. 交互式配置 (`handlers/setup.rs`)

**命令**: `claude-code-sync setup`

**功能**:
- 引导式配置向导（选择同步模式、输入仓库地址）
- 自动安装 gh CLI（如未安装）
- 支持网页 HTTPS 认证
- 自动创建 GitHub 私有仓库（可选）

### 5. 自动更新 (`handlers/update.rs`)

**功能**:
- 启动时后台检查新版本（非阻塞）
- `claude-code-sync update` 手动更新
- `claude-code-sync update --check-only` 仅检查
- 自动下载并替换当前二进制

### 6. 自动同步 (`handlers/automate.rs`, `hooks.rs`, `wrapper.rs`)

**命令**: `claude-code-sync automate`

**功能**:
一键配置自动同步，无需手动执行 push/pull 命令。

**组件**:

1. **Hooks** (`hooks.rs`): Claude Code 原生钩子
   - `SessionStart`: 启动时自动拉取远程历史（IDE 支持）
   - `Stop`: 每轮对话完成后自动推送对话历史
   - `UserPromptSubmit`: 检测新项目并拉取远程历史

2. **Wrapper** (`wrapper.rs`): 启动包装脚本
   - 创建 `claude-sync` 脚本（替代 `claude` 命令）
   - 启动前自动执行 `pull`，确保获取最新历史
   - 支持 Unix (bash) 和 Windows (bat/ps1)

**相关命令**:
```bash
# 一键配置
claude-code-sync automate

# 查看状态
claude-code-sync automate --status

# 卸载
claude-code-sync automate --uninstall

# 单独管理 hooks
claude-code-sync hooks install|uninstall|show

# 单独管理 wrapper
claude-code-sync wrapper install|uninstall|show
```

**工作流**:
```
┌─────────────────────────────────────────────────────────────┐
│                     Auto-Sync Workflow                      │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  [启动] claude-sync                                         │
│     │                                                       │
│     ├─> Wrapper: claude-code-sync pull (拉取最新)           │
│     │                                                       │
│     └─> Claude Code 启动                                    │
│            │                                                │
│            ├─> SessionStart Hook: pull (IDE 启动支持)       │
│            │                                                │
│            ├─> UserPromptSubmit Hook: 检测新项目            │
│            │                                                │
│            └─> Stop Hook: push (每轮对话后推送)             │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

**配置文件位置**:
- Hooks: `~/.claude/settings.json`
- Wrapper: 与 `claude-code-sync` 同目录下的 `claude-sync`

**调试日志**:
```bash
# macOS
cat ~/Library/Application\ Support/claude-code-sync/hook-debug.log
```

### 7. 目录结构一致性检查 (`sync/discovery.rs`)

**功能**: 防止同步模式切换导致的目录混乱

**检测逻辑**:
```rust
pub fn check_directory_structure_consistency(
    sync_repo_projects_dir: &Path,
    use_project_name_only: bool,
) -> DirectoryStructureCheck
```

**警告场景**:
1. 仓库中同时存在完整路径格式 (`-Users-xxx-`) 和项目名格式 (`myproject`)
2. 当前配置模式与现有目录结构不匹配

**触发位置**:
- `push.rs`: 推送前检查
- `filter.rs`: 配置模式变更时
- `setup.rs`: 设置向导中检测模式变更

## 开发规范

### 代码风格

1. **错误处理**: 使用 `anyhow::Result`，提供清晰的上下文信息
2. **日志**: 使用 `log` crate，分级输出（debug/info/warn/error）
3. **测试**: 单元测试放在模块内 `#[cfg(test)]`，集成测试放在 `tests/`
4. **文档**: 公共 API 必须有文档注释 `///`

### 关键原则

1. **跨平台兼容**
   - ❌ 不要使用 `std::path::Path::file_name()` 处理跨平台路径
   - ✅ 使用 `split(&['/', '\\'])` 同时支持两种分隔符

2. **非 ASCII 字符支持**
   - 中文、日文等项目名会被编码为 `-`
   - 必须从 JSONL 内部 `cwd` 字段获取真实项目名
   - 不能假设目录名等于项目名

3. **文件扫描逻辑**
   - 目录中可能有多个 JSONL 文件（对话、快照、子 agent 等）
   - 遇到无效文件时继续尝试，不要提前 `break`
   - 只有匹配失败时才跳过整个目录

4. **性能考虑**
   - 大量对话文件时避免重复解析
   - 使用增量同步而非全量复制

## 重要注意事项

### ⚠️ 中文项目名支持

**问题**: Windows 推送的中文路径在 Mac/Linux 上无法识别

**原因**:
- Windows 路径: `C:\Users\...\安装环境`
- Mac/Linux 的 `Path::file_name()` 不识别 `\`

**解决**:
- 修改 `parser.rs` 和 `sync/discovery.rs`
- 使用 `split(&['/', '\\'])` 同时支持两种路径分隔符

### ⚠️ JSONL 文件类型

目录中的 JSONL 文件包括：
- **对话文件**: 包含完整对话历史，有 `cwd` 字段
- **快照文件**: 文件历史快照，无 `cwd` 字段
- **Agent 文件**: 子 agent 对话，可能在子目录中

**扫描策略**: 遍历所有 JSONL 直到找到有效项目名

### ⚠️ 冲突处理

**场景**: 同一对话在不同设备上被修改

**策略**:
- 保留两个版本
- 重命名：`session.jsonl` → `session-conflict-<timestamp>.jsonl`
- 生成冲突报告

## 常用开发命令

### 构建和测试

```bash
# 开发构建
cargo build

# Release 构建
cargo build --release

# 运行单元测试
cargo test

# 运行集成测试
cargo test --test '*'

# 运行特定测试
cargo test test_extract_project_name

# 带日志输出的测试
RUST_LOG=debug cargo test -- --nocapture
```

### 安装和运行

```bash
# 本地安装
cargo install --path . --force

# 运行并查看详细日志
RUST_LOG=debug claude-code-sync pull

# 查看配置
claude-code-sync config --show

# 查看状态
claude-code-sync status
```

### 代码检查

```bash
# Clippy 检查
cargo clippy -- -D warnings

# 格式化
cargo fmt

# 文档生成
cargo doc --open --no-deps
```

### 发布

```bash
# 交互式发布（选择 push/patch/minor/major）
./scripts/release.sh
```

## 调试技巧

### 启用详细日志

```bash
# 查看项目匹配过程
RUST_LOG=debug claude-code-sync pull 2>&1 | grep "project_name\|MATCH"

# 查看完整调试信息
RUST_LOG=trace claude-code-sync sync
```

### 常见调试点

1. **项目匹配失败**:
   - 检查 `find_local_project_by_name()` 返回值
   - 确认 JSONL 文件是否包含 `cwd` 字段
   - 验证路径分隔符是否正确处理

2. **JSONL 解析错误**:
   - 检查文件格式是否符合 JSONL 规范
   - 查看 `ConversationEntry` 结构体定义
   - 使用 `jq` 手动验证文件: `cat file.jsonl | jq .`

3. **跨平台问题**:
   - 打印 `cwd` 原始值
   - 验证 `project_name()` 提取结果
   - 检查路径分隔符处理逻辑

## 测试策略

### 单元测试

- `parser.rs`: 测试路径解析（Unix/Windows 路径）
- `sync/discovery.rs`: 测试项目名提取和匹配
- `merge.rs`: 测试对话合并逻辑

### 集成测试

- 创建临时目录和 Git 仓库
- 模拟多设备同步场景
- 验证中文项目名处理

### 测试用例示例

```rust
#[test]
fn test_windows_path_on_unix() {
    let session = create_test_session("C:\\Users\\OSEN\\项目名");
    assert_eq!(session.project_name(), Some("项目名"));
}

#[test]
fn test_skip_snapshot_files() {
    // 创建包含快照文件和对话文件的目录
    // 验证能正确跳过快照文件，找到对话文件
}
```

## 文档维护

- **架构变更**: 更新本文档 "架构说明" 部分
- **新增功能**: 更新 README.md 和 `docs/user-guide.md`
- **用户指南**: 见 `docs/user-guide.md`（安装配置、多设备同步、常用命令）
- **配置变更**: 更新配置示例和说明

## 相关资源

- 原始仓库: https://github.com/perfectra1n/claude-code-sync
- 中文 Fork: https://github.com/osen77/claude-code-sync-cn
- API 文档: https://perfectra1n.github.io/claude-code-sync/
- 问题追踪: GitHub Issues

---

*最后更新: 2026-02-03*
