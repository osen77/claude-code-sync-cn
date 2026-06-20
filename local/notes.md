# 项目问题记录

## 2026-06-20: 修复 Open in Claude 的环境变量与别名继承问题

### 问题描述
- 用户在使用交互式菜单 `ccs session` 中选择 "Open in Claude" 时，如果打开命令配置的是 `claude-auto --resume <id>`，会提示找不到 `claude-auto` 命令。但在终端直接执行是可以的。

### 根本原因
- 在 `src/handlers/session.rs` 的 `open_in_claude` 函数中，之前采用了一个硬编码的 workaround 试图加载别名：`zsh -c "source ~/.zshrc && ..."`。
- 该实现存在两个问题：
  1. 用户的 shell 未必是 `zsh`。
  2. 很多用户的 `.zshrc`（或 `.bashrc`）在顶部包含交互式判断（例如 `[[ $- != *i* ]] && return`），导致在 `zsh -c` 的非交互式模式下被直接跳过，NVM、Cargo 路径以及 `claude-auto` 等 alias 和函数无法被加载。

### 解决方案
- 移除了强制写入的 `zsh -c` 逻辑。
- 动态获取环境变量中的当前壳环境：`std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string())`。
- 将子进程启动标志改为 `-ic` (Interactive Command Mode)，这能让 Shell 以为自己运行在终端下，自动加载完整的配置文件。

### 影响范围
- `src/handlers/session.rs`

### 预防措施
- 启动外部命令行工具时，尤其是在 macOS/Linux 下，应始终考虑是否需要继承完整的用户终端配置环境。使用 `$SHELL -ic` 代替显式的 `source` 是更通用且稳健的方式。

## 2026-06-20: 修复交互式菜单中 Rename 操作后列表不刷新的问题

### 问题描述
- 用户在 `ccs session` 的交互式菜单中对某个会话执行 `Rename` (重命名)操作后，按 `Back` 键返回到会话列表时，发现列表中的标题没有更新，仍然显示的是旧标题。

### 根本原因
- 在 `src/handlers/session.rs` 的 `handle_session_interactive` 的循环结构中，当执行 `ActionChoice::Rename` 后，仅仅修改了传入的临时变量 `session` 的 `title`，但并没有触发任何能够引起外层 `all_sessions` 列表重新加载的机制。
- 之前针对 `ActionChoice::Delete` 操作实现了一个 `deleted` 标志位并在结束后重载 `all_sessions` 的机制，但 `Rename` 操作漏掉了相同的机制。

### 解决方案
- 将 `rename_session_interactive` 的返回值从 `Result<()>` 更改为 `Result<bool>`，以便返回重命名是否真实发生的布尔值。
- 在 `SessionMenuChoice::Select` 以及 `Search` 分支内，将 `let mut deleted = false;` 重命名为更具语义的 `let mut list_needs_refresh = false;`。
- 当执行 `ActionChoice::Rename` 且返回 `true`（发生了真实的标题更新）或 `ActionChoice::Delete` 返回 `true` 时，均将 `list_needs_refresh` 设置为 `true`。
- 循环退出后判断 `list_needs_refresh` 并重载 `all_sessions`，以保证上一层级菜单的数据为最新。

### 影响范围
- `src/handlers/session.rs`

## 2026-06-20: 删除语义重构，意图删除与误删保护机制

### 问题描述
- **删除非原子且意图不分**：`ccs session delete` 仅删除本地文件，这导致下次 `pull` 时远端会将文件重新拉回本地（"删了又回来"）。
- **误删污染云端**：如果用户意外丢失或在终端用 `rm` 误删文件，`ccs push` 会将缺失状态同步到云端，把物理丢失变成同步删除。
- **跨设备意图无法传递**：设备 A 删除了某个 session，设备 B 拉取时由于远端只是“少了个文件”，无法判断是该删本地还是因为自己本地有所以把缺失文件再推上去。

### 根本原因
- 系统的 sync 机制（`push.rs` / `pull.rs`）对于删除采用的是启发式的差异推平逻辑：只要本地少了就推给远端删，只要远端多了就拉给本地存。
- 没有地方记录“为什么要删除”。

### 解决方案
引入了 Tombstone（删除登记册）和删除语义重构，使整个删除从启发式变成“基于 Git 和协议文件的确定性保护”：
1. **tombstone 模块 (`src/sync/tombstone.rs`)**：
   在 `sync_repo` 内增加 `.ccs/deletions.json`。登记册伴随 commit 在设备间传播，彻底消除了同步二义性。
2. **重构删除核心 (`src/handlers/session.rs`)**：
   引入 `delete_session_with_commit`。单次或批量删除现在是原子操作：删本地 + 同步删云端库文件 + 写 tombstone 登记册 + 一次 Git commit（`delete(session): explicit <id>` 或 `cleanup(session): <N> garbage sessions`）。
3. **Push 保护模式 (`src/sync/push.rs`)**：
   当 `ccs push` 发现本地缺失但在云端依然存在的文件，默认作为“误删”保护起来（拦截并拒绝删除云端），只输出告警。
4. **强删参数 (`ccs push --prune`)**：
   为逃生舱设计的强行同步缺失文件的选项。
5. **意图传播 (`src/sync/pull.rs`)**：
   在 `ccs pull` 合并完成后，应用 tombstone——如果是记录在案的意图删除，就跟着移除本地文件。
6. **灾难恢复 (`ccs restore`)**：
   新增交互式和非交互式恢复命令，可以精准扫描那些在保护模式中被云端挽救的误删会话并复制回本地。

### 影响范围
- `src/sync/tombstone.rs` (新增)
- `src/handlers/session.rs` (重写 delete 逻辑，增加 restore 子命令)
- `src/sync/push.rs` (删除拦截及 --prune 透传)
- `src/sync/pull.rs` (应用 tombstone)
- `src/main.rs` (新增 `SessionAction::Restore` 和 `--prune` flag)

### 预防措施
- 核心代码均增加对应的单元测试以确保覆盖。
- 考虑到跨平台的潜在问题和 future multi-repo 架构演进，tombstone 文件独立设计而非强耦合 `state.json` 或 `history.json`。

## 2026-06-19: Multi-device concurrent push silently diverged

### 问题描述
- 两台设备几乎同时执行 `ccs push` 时，后发设备的 `git push` 被 non-fast-forward 拒绝。
- `src/sync/push.rs` 仅记录 warning，但仍向用户显示 push 完成，导致静默分叉和后续持续失败。

### 根本原因
- push 流程没有 pull/rebase/retry 闭环。
- `SyncState` 不记录上次成功同步 commit，无法主动发现漂移。
- Stop hook 使用 `ccs push --quiet`，放大了静默失败问题。

### 解决方案
- 为 git SCM 增加 push 错误分类、fetch、rebase、rebase cleanup helpers（src/scm/）。
- 用 bounded retry 的 `push_with_rebase_auto_heal` 替换直接 push（src/sync/push.rs）。
- 在 state.json 中记录 `last_synced_commit`，用于漂移诊断（src/sync/state.rs）。
- rebase 冲突时 fallback 到 keep-both 文件副本，避免数据丢失。

### 影响范围
- `src/scm/mod.rs`, `src/scm/git.rs`
- `src/sync/push.rs`
- `src/sync/state.rs`

### 预防措施
- 为后台静默命令（如自动触发的 hooks）提供更显式的非零退出和重试机制，或者向用户推送 Notification（后续可结合系统通知完善）。
