# 项目问题记录

## 2026-08-03：普通 pull、tombstone 与 Release 发布完整性加固（TDD，未 commit/push）

### 问题描述
- 最终相邻路径复审发现：普通 session pull 可跟随本地 project/file symlink 写出 Claude projects root；tombstone propagation 可跟随本地 symlink 删除 root 外文件；`.ccs/deletions.json` load/save 可被同步仓库中的 directory/file symlink 导向仓库外。
- Release 在五平台构建完成前已公开，失败会留下部分资产；release build 未使用 `--locked`。

### 根本原因
- auto-memory 与 session delete 已接入共享 path-security guard，但普通 pull 写入、交互冲突写入和 tombstone registry/propagation 仍保留原始 `join`、`is_dir`、`File::create`、`remove_file` 调用链。
- Release job 把创建公开 Release 放在矩阵构建之前，缺少最终资产集合验证与 draft 发布门禁。

### 解决方案
- 新增通用 `validate_directory_root()` 与 `prepare_regular_file_destination()`，明确拒绝 root 自身 symlink；从可信 root 和 relative path 重建目标，创建父目录后验证 non-symlink containment，并在写入前再次复核；普通 pull 与交互冲突（smart merge/keep remote/keep both）统一使用该边界。
- tombstone propagation 对 root/project/file 使用 `symlink_metadata`、directory/regular candidate guard，删除前重新构造并验证 root-relative candidate。
- tombstone registry 验证 repo、`.ccs`、`deletions.json`，拒绝 symlink/non-regular 对象，并通过同目录 `NamedTempFile` + `sync_all` + atomic persist 保存。
- 普通 pull/status 在扫描远端 session 前验证 sync projects root；push rebase conflict scanner 跳过目录/文件 symlink；legacy 与缓存版 session scanner、project matching/collision/列表扫描拒绝 CC/CX/OM source root 及 local project/file symlink。
- memory search 验证 project root、memory directory 和 Markdown file 均为 containment 内 non-symlink regular candidate，不读取外部链接目标。
- Release 先创建 draft，五平台使用 `cargo build --release --locked`；每个资产上传显式保持 `draft: true`，最终 job 核对五个归档和五个 SHA256 后才发布。

### 影响范围
- `src/path_security.rs`、`src/sync/pull.rs`、`src/sync/tombstone.rs`、`src/interactive_conflict.rs`、`src/conflict.rs`、`.github/workflows/release-new.yml` 及 tempfile symlink 回归测试。

### TDD 与验证
- RED 对应旧实现可覆盖/删除外部 marker，或通过 `.ccs` symlink 写出仓库；新增测试固定 project/file/directory symlink 攻击面。
- GREEN 需通过 pull/tombstone/conflict focused tests、宿主与 Windows GNU check/clippy、全量串行测试、release build、workflow 静态检查和 `git diff --check`。

### 预防措施
- 所有 session/tombstone 文件写删必须从可信 root + validated relative path 重建；发布只允许在完整资产集合校验后由 draft 转为公开。

## 2026-08-03：最终发布审查阻断修复（未 commit/push）

### 问题描述
- 最终累计审查发现：auto-memory pull 可跟随远端或本地 project/memory/file symlink；非交互 rename/delete 在 degraded scan 上静默继续 mutation；手动 Release workflow 的资产 tag、token 权限和 Cargo 版本校验不完整；自动更新资产选择未区分 musl；部分测试和 Hook 仍存在跨平台隔离/路径问题。

### 根本原因
- push、pull 的 auto-memory 早期实现没有共享同一组 source/destination root guard；mutation入口复用了 summary-only兼容wrapper；release workflow在tag push路径可工作但没有统一解析manual dispatch tag；平台分支只在宿主机验证。

### 解决方案
- auto-memory pull 对remote projects root、project/memory/file和local project/memory/destination逐级执行non-symlink containment与最终重验；push同时验证local project/memory。
- 非交互rename/delete消费report-aware scan，输出diagnostic并在degraded时fail-safe中止。
- Release workflow统一输出release tag、checkout同一tag、校验Cargo版本、上传到同一tag并声明`contents: write`。
- updater资产映射与五资产发布矩阵对齐，musl选择musl资产，不支持的平台返回明确错误。
- onboarding/integration测试使用temp config+RAII+`#[serial]`；Hook debug日志改用跨平台config目录，Windows进程检测使用`tasklist`。

### 影响范围
- `src/sync/pull.rs`、`src/sync/push.rs`、`src/path_security.rs`、`src/handlers/session.rs`、`src/handlers/update.rs`、`src/handlers/hooks.rs`、`src/main.rs`、Release workflow及隔离测试。

### 预防措施
- 发布前必须运行宿主与Windows GNU `check/clippy --all-targets`、全量串行测试、release build、workflow静态检查，并对tag与Cargo version做自动一致性校验。

## 2026-08-03：auto-memory 本地目录 symlink 越界读取与误删修复（TDD，未 commit/push）

### 问题描述
- auto-memory 仅加固了同步仓库目标路径；本地 `<project>/memory` 仍通过 `Path::is_dir()` 和 `read_dir()` 跟随 symlink。恶意或误配置 symlink 可把仓库外文件复制进同步仓库；若外部目录为空，还可能把远端 memory 文件误判为本地删除并 prune。

### 根本原因
- `sync_auto_memory_directories()` 没有接收可信的 Claude projects root，也没有验证 local project/memory 的现有路径组件和最终目录类型。

### 解决方案
- 新增共享 `validate_directory_candidate()`，要求目录 candidate 为 root 内 regular non-symlink directory，且所有现有组件不含 symlink。
- auto-memory helper 显式接收 Claude projects root，在读取、复制或建立 prune 集合前验证 local project 与 memory；NotFound 继续视为没有 memory，symlink/越界则 fail-safe 终止。

### 影响范围
- `src/path_security.rs`、`src/sync/push.rs` 及 auto-memory tempfile 回归测试。

### TDD 与验证
- RED：local project/memory symlink fixture 会复制外部 `secret.md`，测试按预期失败。
- GREEN：修复后不复制外部文件、不删除远端 marker；auto-memory focused 15 tests、本机和 Windows GNU check/clippy、fmt、diff check均通过。

### 预防措施
- auto-memory 的 source 与 destination 都是文件系统信任边界；后续改动必须分别携带可信 root并使用共享 directory/file candidate guard。

## 2026-08-03：补齐 Windows cross-target 编译门禁（未 commit/push）

### 问题描述
- 安装 `x86_64-pc-windows-gnu` target 后，生产代码可完成 `cargo check`，但 `--all-targets` 被 Unix-only 测试引用阻断；Windows Clippy 同时发现 Unix wrapper 符号 dead code、平台分支未使用参数和 `needless_return`。

### 根本原因
- Unix 专用常量、helper 和权限测试缺少精确 `#[cfg(unix)]`；symlink fixture 在非 Unix 平台提前声明但不会使用；Windows 成功提示分支不消费仅供 Unix alias 展示的 wrapper path。

### 解决方案
- 对 Unix wrapper 常量/helper和 mode 权限测试增加精确平台条件；把 symlink fixture 路径移入 Unix 块；Windows 分支显式消费参数；移除 Windows 分支多余 `return`。

### 影响范围
- 仅平台条件、测试可编译性和 lint 清洁度，不改变 Unix/Windows 运行行为。

### 预防措施
- 发布前同时执行当前平台与 `x86_64-pc-windows-gnu` 的 `cargo check/clippy --all-targets -- -D warnings`，避免仅凭宿主平台绿色判断跨平台可发布。

## 2026-08-03：路径安全终审唯一 Important 调用链修复（TDD，未 commit/push）

### 问题描述
- 终审确认 session 单删、batch cleanup 仍可直接以 `sync_repo/projects` 拼接删除目标，未统一执行严格 projects-root guard；auto-memory push 仍对 project/memory/file 使用 raw join、create/copy/read/remove，恶意 checkout symlink 可能把写入或删除导向仓库外。

### 根本原因
- `safe_join_within_root()` 的通用 scanner 边界语义不会替代 sync-repository 专用 `validate_sync_projects_root()`；删除调用链和 auto-memory 分支没有在每个最终 filesystem operation 前重建 validated relative path 并复核 root/candidate。

### 解决方案
- 新增共享 `safe_join_within_sync_projects_root()`，统一执行严格 root guard 后再复用 lexical、existing-component containment。
- session 单删与 batch cleanup 在删除本地 session 前先预检 sync root；同步文件删除时从 validated relative path 重建，并在 `remove_file` 前重新执行 strict root、safe join、regular non-symlink candidate 校验。
- auto-memory push 抽为共享 guarded helper；root/project/memory/file destination 的 create/copy/read/remove 全部使用 sync-root helper，最终操作前 fail-safe 重验；symlink/non-regular candidate 一律拒绝，不访问外部 marker。
- 保留同 UID check-then-use TOCTOU 作为已记录 residual，未做 openat/O_NOFOLLOW 大规模平台重构。

### TDD 与验证
- RED：先加入 tempfile 回归测试；auto-memory helper 尚不存在时 focused compile 失败，证明测试先于实现；session tests 初始复现了 root symlink 调用链风险。
- GREEN：实现共享 guard、delete/batch preflight 与 auto-memory guarded helper 后通过 focused tests。
- 覆盖：正常 auto-memory 写入；sync projects root、project directory、memory directory、destination file symlink；单删和 batch cleanup root symlink；所有外部 marker 保持不变。
- 测试 fixture 仅使用 tempfile 隔离目录和测试环境变量，未访问真实 `~/.claude`、`~/.codex`、`~/.omp/config/log`。

### 影响范围
- `src/path_security.rs`、`src/handlers/session.rs`、`src/sync/push.rs` 及对应单元回归测试；未 commit、push 或 release。

### 预防措施
- 所有 sync-repo projects root 读写删必须复用 `safe_join_within_sync_projects_root()` 与 regular candidate guard；最终 filesystem operation 前必须重新构造并复核 validated relative path。

## 2026-08-03：路径安全终审 Important/Minor 修复（未 commit/push）

### 问题描述
- 终审发现同步仓库的 `projects` 根目录自身可为 symlink，导致 push 写入、restore 读取和 prune 删除可能越出同步仓库；旧 cache v3 已知 source 的 raw lexical key 还可能与 canonical key 并存。

### 根本原因
- 普通 `safe_join_within_root()` 为支持 scanner root alias，会把 root canonicalize 后作为边界，未拒绝 root 本身 symlink；prune 直接使用 `read_dir` 原始路径删除；cache load/merge 未迁移已知 source 的旧 raw key。

### 解决方案
- 新增 `validate_sync_projects_root()`：要求 projects root 是 non-symlink directory，且 canonical projects root 位于 canonical sync repository root 内；push、restore、prune 共用该 guard。
- prune 仅保存 validated relative path，之后从受信 root 重建并再次验证 regular non-symlink candidate 后删除。
- cache v3 load/merge 阶段迁移已知 source 的 existing regular raw key 到 canonical UTF-8 key，按 `(source, canonical key)` 去重；unknown、无法 canonicalize 和非 regular 条目 fail-safe 保留，不升级 schema。

### 影响范围
- `src/path_security.rs`、`src/sync/push.rs`、`src/handlers/session.rs`、`src/session_cache.rs` 及对应 tempfile 回归测试；未触碰真实用户数据。

### 验证与限制
- 通过 root-symlink push/prune、restore 不读外部文件、raw-to-canonical dedup focused tests；`cargo check --all-targets`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`cargo test --all-targets -- --test-threads=1`、`cargo build --release`、`git diff --check` 均通过。
- 仍保留同 UID 攻击者在最终检查与 filesystem 操作之间替换路径的 TOCTOU 残余；当前 Rust 跨平台 API 未提供覆盖全流程的 openat/O_NOFOLLOW 原子句柄语义，因此未宣称完全消除竞态。

### 预防措施
- 任何同步仓库 root、restore source/destination 或 prune 删除改动必须复用严格 root guard，并以 validated relative path 重建最终操作路径；cache identity 改动必须保留 v3 兼容及 unknown/fail-safe retention。

## 2026-08-03：logger rereview F1-F5 修复（未 commit/push）

### 问题描述
- rereview 发现日常 file write 未参与 per-log lock、lock/chmod 缺少原子 no-follow 边界、hosted file URI 与 escaped/multiline quoted credential 覆盖不足、legacy target warning 绕过 detail cap、rotation failpoint 可能污染并行测试。

### 根本原因
- logger 持有初始化时打开的 File，轮转后长生命周期进程继续写旧 inode；权限操作使用 path API；warning cap 只在 ScanDiagnostics 实例内；failpoint 为共享进程状态。

### 解决方案
- file sink 改为每次 write/flush 在同一 per-log lock 内 fresh no-follow 打开 current；轮转拆分 locked wrapper，并保留 rollback transaction。
- Unix 使用 O_NOFOLLOW，Windows 编译分支使用 FILE_FLAG_OPEN_REPARSE_POINT；权限通过已打开 handle 设置。
- sanitizer 支持任意 host file URI，有限状态扫描 escaped quote/control/multiline quoted credential。
- logger 对 SCAN_DIAGNOSTICS_TARGET 建立 per-invocation bounded budget；超 cap 一次性固定 suppressed record，JSON counters 不变。
- failpoint 改为 thread-local context + RAII reset；新增长生命周期 A/B/A 跨进程轮转回归与平台 flag 静态测试。

### 影响范围
- 仅 logger、session diagnostics、logger CLI tests、README/用户指南、notes/report；未修改 session.rs/sync callsites，未触碰真实日志、配置或 session 数据。

### 验证与限制
- logger 33、diagnostics 12、CLI 8 focused tests 通过；all-target Clippy、full fmt、full cargo test、release check、scoped rustfmt、diff check 全部通过。
- 未执行 commit、push 或 release；仍保持后续 session/sync callsite 隐私工作延期。

### 预防措施
- 所有 file sink 后续改动必须验证 lock 生命周期、fresh no-follow open 与跨进程 rotation；warning target 统一经过 logger budget。

## 2026-08-03：logger 发布阻断加固（TDD，未 commit/push）

### 问题描述
- logger 审查发现轮转在删除/逐代 rename 中途失败会丢失 current 或既有 generation；runtime sink 的 write、flush、Mutex poison 静默丢错；symlink 日志路径会跟随并修改外部 target。
- sanitizer 对 `file://` URL、带空格的引号 token 和双引号边界覆盖不足；public `invocation_id` 与 diagnostics ID 可注入控制字符；warning detail cap 没有限制 file log；legacy `init_logger` 会丢弃 file-sink warning。

### 根本原因
- 旧轮转直接修改 live 路径，缺少 staging、事务备份与 rollback；sink I/O 只使用忽略错误的 `let _`，锁 poison 直接跳过。
- 路径脱敏先保护 URL scheme，导致 `file:///...` 绕过绝对路径规则；secret regex 只消费到空格前；ID 边界只依赖默认 UUID 构造器。
- diagnostics cap 只限制 JSON detail vector，仍逐条调用 `log::warn!`。

### 解决方案
- 在 per-log lock 内采用 staging copy + transaction rename + rollback；所有 current、lock、generation 通过 `symlink_metadata` 检查，Unix append 使用 `O_NOFOLLOW`，失败保持安全可见。
- DualLogger 为每个 sink 增加 write/flush/poison 计数及一次性固定 stderr fallback；格式边界再次验证 invocation ID，`ScanDiagnostics::with_id` 复用安全 helper。
- sanitizer 增加 file URL path、完整单/双引号 secret 回归；warning cap 后只输出一次固定 suppressed file record，JSON suppressed 计数不变；legacy init 显式输出固定 file-sink warning。

### 影响范围
- `src/logger.rs`、`src/session_diagnostics.rs`、`tests/logger_cli_tests.rs`、README/用户指南及本记录；按要求未修改 `session.rs` 或 sync callsites，未触碰真实日志/配置/session 数据。

### 验证与限制
- 已补充 rotation 中途注入失败 rollback、failing writer/flush/poison、symlink、sanitizer、ID、warning cap 和 CLI 隔离测试。
- `cargo fmt`/diff whitespace 检查可执行；当前工作区存在本任务开始前的 session/path-security 未完成改动，导致 focused/full Cargo test 与 clippy 在 crate 编译阶段被既有错误阻断，未将这些无关文件纳入本次修复。

### 预防措施
- 后续 session/sync callsite 修复 session ID/relative path 的隐私策略；本 logger sanitizer 不对无法可靠识别的相对路径做破坏性泛化。
- 发布前需在基线编译恢复后重跑 logger/diagnostics focused、CLI、clippy、fmt、full test、release check，并核对 `git diff --check`。

## 2026-08-02：完成 Session Cache Correctness Slice Task 4 并发与原子性门禁

### 问题描述
- 旧的全局 prune 语义会让 Claude-only/Codex-only 等 source-filtered scan 误删未选择来源的 cache entry。
- 多进程 writer 若各自基于旧快照整包保存，会发生 lost update；普通 truncate/write 还可能让 reader 读到半截 JSON。
- 目录缺失、遍历不完整或 metadata/fingerprint 失败时，若被当成“扫描完成”，会错误 prune 仍存在的历史。

### 根本原因
- cache retention 没有按 source 记录 selected、seen 和 completed 状态，无法区分“未选择/未完成”和“确认不存在”。
- read-modify-write 缺少独立 lock file 与 latest reload；持久化直接覆盖 target 时 reader 没有完整 JSON 边界保证。
- 删除判断缺少 merge 阶段的 NotFound revalidation，未建立 cache advisory 的 fail-safe 边界。

### 解决方案
- 增加隔离 CLI retention 回归与跨进程双 writer 测试，覆盖 All 建 cache、Claude-only/Codex-only 依次及并发执行，并验证最终 JSON 可解析、两个 source entries 同时保留。
- 增加独立 child lock holder 测试：marker 表示锁已取得，writer 在 release marker 前不得完成，释放后成功。
- 增加 atomic reader stress：writer 重复 atomic save，reader 循环只接受旧/新完整结构，parse error 必须为 0。
- 文档明确 source-aware retention、incomplete fail-safe、confirmed NotFound prune、lock/delta latest merge、同目录 atomic replace、Windows persist 与 advisory cache 边界。

### 影响范围
- `tests/session_cache_concurrency_tests.rs`、`tests/session_scan_diagnostics_tests.rs`、`README.md`、`docs/user-guide.md`、本记录；未 commit、未 push，测试只使用临时 HOME/USERPROFILE/config/session roots/log。

### 预防措施
- 新增来源或 source filter 必须保留 source 分区 retention，未选择来源不能进入 prune。
- 所有 cache mutation 必须经过独立 lock、latest reload、delta merge 和 NotFound revalidation；写入必须 atomic replace。
- 并发测试使用 marker/barrier，不以固定 sleep 作为唯一同步；cache 继续视为可丢弃索引，不能作为会话备份。

## 2026-08-02：完成 Slice 3 最终两项修复

### 问题描述
- Claude legacy discovery 的 parser warning 仍可能把路径和底层错误正文写入普通 logger；scanner 缺少同 size、同 mtime、不同内容的真实 BLAKE3 回归门禁。

### 根本原因
- `src/sync/discovery.rs` 使用了带 path/raw error 的默认 target warning；已有 cache fingerprint 单测未贯穿 `scan_all_session_summaries_with_roots` 的 clean→内容替换→warm scan 流程。

### 解决方案
- Claude legacy parser warning 改为 `SCAN_DIAGNOSTICS_TARGET` 和固定安全文案，不再包含 path/raw error，并增加契约测试。
- 增加 Claude scanner unit test：首轮 clean scan 写 cache，保存 modified 后写入同长度 malformed 内容，用 `FileTimes`/`set_times` 恢复 mtime；二次 scan 断言 `cache_hits=0`、`cache_misses=1`、`degraded=true`、无旧 summary 且 cache entry 被删除。

### 影响范围
- `src/sync/discovery.rs`、`src/handlers/session.rs` 及本地测试记录；未触碰真实用户 session/config/log 数据。

### 预防措施
- 新测试按 RED→GREEN 执行，保留 discovery/session/cache focused tests，并执行 fmt、all-target Clippy、全量 cargo test 与 diff check。

## 2026-08-02：完成 Slice 3 第二轮最终修复

### 问题描述
- 第二轮审查发现 session index cache 仅依赖 size+秒级 mtime，可能错误复用同元数据但内容已改变的文件；partial/parser error 也可能保留旧 clean entry。另有 interactive/legacy discovery 的非目录与 WalkDir 错误处理不一致，以及日志 sanitizer/fallback 在部分 colon、Windows 路径和底层错误场景下存在泄露风险。

### 根本原因
- scanner 在 metadata 后没有验证内容 fingerprint；cache eviction 只覆盖部分分支；legacy 扫描使用静默 `filter_map(|e| e.ok())`；日志路径匹配边界未覆盖 `path:/...`、盘符正斜杠、反斜杠和 UNC，fallback 直接拼接了 log path/raw error。

### 解决方案
- cache version 从 2 提升到 3，候选文件执行流式 BLAKE3 fingerprint，命中同时校验 size、mtime、fingerprint；新增 `fingerprint_ms` 与 `fingerprinted_bytes`，fingerprint I/O 失败跳过候选并记录受控 warning。
- Claude/Codex/OMP 的 partial 与 parser error 分支统一删除旧 cache entry；补充 clean→partial→warm 三来源回归测试，证明已有 clean entry 不会被错误命中。
- regular-file root、interactive detection、Claude/Codex/OMP legacy WalkDir error 均改为 best-effort 并记录受控诊断；移除静默丢错路径。
- sanitizer 覆盖 colon path、Windows 两种盘符格式、UNC 与 quoted path；logger fallback 和 CLI stderr 改为固定安全文案，不泄露路径或底层错误。

### 影响范围
- `src/session_cache.rs`、`src/handlers/session.rs`、`src/session_diagnostics.rs`、`src/sync/discovery.rs`、`src/codex.rs`、`src/omp.rs`、`src/logger.rs`、`src/main.rs` 及对应测试与用户文档。

### 预防措施
- 保留 focused scanner/cache/logger gates，并增加已有 clean cache 转为 partial 后三来源 eviction 的回归覆盖；最终执行 fmt、all-target Clippy、全量测试与 `git diff --check`，不接触真实用户 session/config/log 数据。

## 2026-08-02：完成 Slice 1–3 最终审查修复

### 问题描述
- 扫描 JSON 的 degraded 状态此前未显式输出；部分 JSONL 坏行可能保留 summary 却被 clean cache 隐藏；三个来源的非目录 root、全局日志绝对路径脱敏和阶段指标覆盖不足。

### 根本原因
- 诊断结构只有由 counters 计算的 accessor，没有安全序列化字段；parser 兼容 API 直接返回 session，scanner 无法观察部分解析；source root 只检查 metadata 存在；统一 logger 只覆盖部分目标/路径场景；search handler 未记录 full-load 阶段。

### 解决方案
- 为 diagnostics 增加 computed `degraded: bool` 与 10 个 flat phase metrics，保持 schema version 1。
- 引入共享 `ParseOutcome<T>` 和三个 report API；坏行保留有效 summary、记录 data warning、禁止写 cache，并将 cache version 从 1 提升到 2。
- Claude/Codex/OMP root 统一要求 `metadata.is_dir()`；抽取共享 WalkDir error helper。
- DualLogger 统一对 console/file sink 脱敏 home 外 Unix/Windows 绝对路径，同时保留 credential、URL userinfo、home、newline 规则。

### 影响范围
- 扫描 JSON、文本 degraded warning、session index cache、三来源扫描计数/耗时、普通 logger target 与文档；兼容保留三个既有 `from_file` API。

### 预防措施
- 新增 parser、诊断、缓存不写入、非目录 root、WalkDir、普通 target 双 sink 和 CLI 隔离 fixture 测试；执行 all-target Clippy 与完整测试门禁。

## 2026-07-03: 新增删除放行窗口 `ccs unlock-delete`

### 问题描述
- 用户有时用 `rm`/文件管理器/外部服务有意删除 session，但 push 保护模式会拦截，且 Stop hook 自动 push 不带 `--prune`，导致有意删除永远同步不上云。

### 解决方案
- 新增 `src/sync/delete_unlock.rs`：`config_dir/delete-unlock.json` 存到期 unix 时间戳，被动过期、无后台进程。
- `push.rs` 新增纯函数 `decide_missing_action`，`missing_in_repo` 分支消费窗口状态：窗口生效时等价 `--prune`（不写 tombstone），打印醒目 🔓 提示；显式 `--prune` 优先且保留原文案。
- 新增 `ccs unlock-delete [--minutes N|--off|--status]`（默认 15 分钟），开启后按中国时区展示到期时刻。
- `is_active()` fail-safe：状态文件损坏/缺失一律回退保护，绝不误删。

### 影响范围
- 新增 `src/sync/delete_unlock.rs`、`src/handlers/unlock_delete.rs`；改 `config.rs`、`sync/mod.rs`、`sync/push.rs`、`handlers/mod.rs`、`main.rs`。

### 预防措施
- 单测覆盖：`remaining_at` 纯函数、unlock/disable/status 往返、坏文件 fail-safe、`decide_missing_action` 三态；CLI 各路径隔离实跑验证。

## 2026-07-03: 修复 `ccs session show --around` 定位不准（总从头显示）

### 问题描述
- 按标准检索流程 `session search "<词>"` 找到 session → `session show <id> --around "<词>"` 钻取上下文时，`--around` 常从会话开头显示，而非定位到关键词处，钻取失效。

### 根本原因（两个缺陷叠加）
1. **内容源不一致（主因）**：`search`（`search_sessions_full`）用 full 内容提取；`show --around`（终端默认，无 `--json`/`--full`）用 simplified 提取。simplified（`simplify_text_content`）会删除围栏代码块内容、并将每个 text block 截断到 500 字符。于是 search 命中的词若落在代码块内 / 500 字符后 / 工具输出里，在 simplified 内容中不存在。
2. **匹配失败静默回退（放大器）**：`handle_session_show` 中 `.position(...).unwrap_or(0)` 匹配失败时回退到位置 0，且 `showing` 照常输出 `around:"kw":n`，伪装成定位成功，用户无从察觉 → 表现为"总从头"。
- 实证：定位算法本身没问题（simplified 能命中的词如"打个新 Tag"→[14] 定位准确），问题纯在内容源与失败处理。

### 解决方案
- `--around` 强制用完整内容：`collect_display_messages_for_summary(session, json || full || around.is_some())`，与 `search` 的 index 体系对齐。
- 抽出纯函数 `find_around_range()`，未命中返回 `None`（不再 `unwrap_or(0)`）。
- 未命中显式提示：终端打印"未在会话中找到关键词: X"；JSON 输出 `showing` 带 `:not-found`、`messages` 为空。

### 影响范围
- `src/handlers/session.rs`（`handle_session_show` + 新增 `find_around_range`）、`src/parser.rs`（回归测试）。
- 行为变更：`--around` 输出改为完整内容（含代码块、不截断），正是该场景所需；`--tail`/`--head`/默认视图不受影响。

### 预防措施
- 新增单测锁死根因：`test_simplified_drops_keywords_that_full_keeps`（parser.rs，证明两种提取的内容差异）+ 4 个 `find_around_range` 用例（含未命中返回 None）。
- 教训：凡"用户输入关键词做匹配定位"的功能，匹配所用内容源必须与用户找到该词的入口（search）一致；匹配失败禁止静默回退，须显式反馈。

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

## 2026-07-04: 修复 Stop hook 子调用 PATH 失败导致自动同步失效

### 问题描述
- Stop hook 静默失败，`hook-debug.log` 连续出现 `Stop push failed to execute: No such file or directory (os error 2)`，对话历史未被自动推送到同步仓库。
- 表象：`throttled-stop.sh` 每次都"成功"（退出 0）并更新节流时间戳，但实际 push 从未执行。

### 根本原因（两层缺陷叠加）
1. **spawn 用裸命令名**：`handle_stop`/`handle_session_start`/`handle_new_project_check` 三处用 `Command::new(BINARY_NAME)`（`BINARY_NAME = "ccs"`，裸名靠 PATH 解析）spawn 自身子命令。Claude Code 的 Stop hook 执行环境 PATH 受限，不含 `~/.cargo/bin`，导致 spawn 失败（`os error 2`）。
2. **错误被吞没**：`handle_stop` 拿到 push 失败的 `Err` 后只写日志，仍返回 `Ok(())`。`throttled-stop.sh` 收到退出码 0 误判成功、更新 `/tmp/ccs-last-push`，导致 5 分钟内后续 Stop 全部被节流跳过，push 永远不会重试。
- 注：`config_sync` 是直接函数调用（非 spawn），不受 PATH 影响——这解释了为何 00:37 hook push 失败但 config 提交成功。

### 解决方案
- 新增 helper `spawn_ccs_subcommand`：用 `std::env::current_exe()` 取当前 ccs 二进制绝对路径 spawn 子命令，完全脱离 PATH 依赖；`current_exe()` 失败时 fallback 到 `BINARY_NAME`（不劣于旧逻辑）。
- 三处 `Command::new(BINARY_NAME)...status()` 替换为 `spawn_ccs_subcommand(...)`：`handle_new_project_check`(pull)、`handle_stop`(push)、`handle_session_start`(pull)。
- `handle_stop` 末尾按 push 结果返回：`Ok(status.success()) → Ok(())`，其余返回 `Err`。`ccs push` 无变更时返回 `Ok`（退出 0），故不会因"无变更"误触发失败；失败时 `throttled-stop.sh` 收到非 0 → 不更新时间戳 → 下次 Stop 立即重试。
- 单测 `spawn_ccs_subcommand_returns_result_without_panic` 验证不 panic、返回 Result（test 环境 `current_exe()` 指向 test binary，退出行为无意义可断言，真正的验证靠实跑）。

### 影响范围
- `src/handlers/hooks.rs`：新增 helper、改三处 spawn、改 `handle_stop` 错误传播、加单测。
- 版本：0.4.8（未发版）。

### 验证
- `cargo build` / `cargo clippy -D warnings` / `cargo test`（14 passed）全绿。
- 实跑 `ccs hook-stop`：日志从 `failed to execute` 变为 `Stop push completed: exit code exit status: 0`。
- 实跑 `throttled-stop.sh`（清空节流）：完整链路通畅，`/tmp/ccs-last-push` 正确更新，日志 `completed`。

### 预防措施
- spawn 自身子命令一律用 `current_exe()`，禁止裸 `BINARY_NAME`（hook 环境不可假设 PATH）。
- 后台/静默路径的错误必须传播，禁止"写日志 + 返回 Ok"——否则上游节流/重试逻辑会被误导。
- 仍待办：`get_hooks_config()` install 时写入 settings.json 的命令字符串仍是裸 `ccs hook-*`（Claude Code 直接执行，不走 `current_exe`）；用户已手动把 Stop 指向 `throttled-stop.sh`（绝对路径）规避，SessionStart/UserPromptSubmit 未报告失败，暂不动以免扩大影响面。

## 2026-07-05: 根治 hook 配置 command not found: ccs（写入侧）

### 问题描述
- `~/.claude/settings.json` 中由 `ccs hooks install` 写入的 SessionStart/UserPromptSubmit 命令为裸 `ccs hook-session-start` / `ccs hook-new-project-check`。Claude Code 用受限 PATH 的 shell 执行 hook（不含 `~/.cargo/bin`），报 `command not found: ccs`，hook 静默失效。
- 与 2026-07-04 是同源 PATH 假设问题，但发生在「配置写入」侧而非「spawn」侧：前者修了运行时 spawn，本次修 install 写入 settings.json 的命令字符串。

### 根本原因
- `get_hooks_config()` 拼命令字符串时直接用 `BINARY_NAME`（裸名 `"ccs"`），依赖执行环境 PATH。
- install 流程只 append 不刷新：旧设备已存在的裸命令 hook 不会被升级为绝对路径，重装也修不好。

### 解决方案
- 新增 `hook_command(subcommand: &str) -> String`：用 `std::env::current_exe()` 取绝对路径，双引号包裹（应对含空格路径，Windows/macOS 通用），失败时 fallback `BINARY_NAME`（不劣于旧逻辑）。
- `get_hooks_config()` 三处（session-start / stop / new-project-check）改用 `hook_command()`。
- 新增 `update_our_hook_command(existing: &mut [Value], subcommand, new_command) -> bool`：在 install 时对已存在的「我们的 hook」（marker 为 cmd 含 `ccs`/`claude-code-sync` 且含对应 subcommand）就地刷新 command 字符串为绝对路径，**保护**用户自定义 wrapper（如 `throttled-stop.sh`，不命中 marker 不动）。
- `handle_hooks_install` 合并循环：subcommand 提取从 `split_whitespace().nth(1)` 改为 `find(|t| t.starts_with("hook-"))`（兼容带空格的引号路径）；命中刷新打印 `↻ refreshed (absolute path)`，否则 append。
- 4 个单测：`hook_command_is_quoted_absolute_path`、`update_our_hook_command_refreshes_bare_command`、`update_our_hook_command_ignores_custom_wrapper`、`subcommand_extracted_from_quoted_spaced_path`。

### 影响范围
- `src/handlers/hooks.rs`：`hook_command`、`update_our_hook_command`、`get_hooks_config`、`handle_hooks_install`、4 单测。
- 版本：0.4.8 → 0.4.9（未发版）。

### 验证
- `cargo test`（hooks 模块 5 passed，全量 270+ passed）、`cargo clippy -D warnings`、`cargo fmt --check`（hooks.rs）全绿。
- 端到端：`CLAUDE_CODE_SYNC_CONFIG_DIR` 复用真实 config + 隔离 HOME 跑 `ccs hooks install`，确认 settings.json 中裸命令被刷新为 `"/Users/mini/.cargo/bin/ccs" hook-*` 形式，`throttled-stop.sh` 不被触碰。
- 当前 Mac `~/.claude/settings.json` 已手动标准化（备份 `settings.json.ccs-bak-20260705`），仅改 SessionStart/UserPromptSubmit 两行，Stop 保持 `throttled-stop.sh`。

### 预防措施
- 配置写入的命令字符串一律走 `hook_command()`（current_exe 绝对路径 + 引号），禁止裸 `BINARY_NAME`。与 2026-07-04 的 spawn 侧规则合并为「hook 环境不可假设 PATH，无论 spawn 还是写入」。
- install 必须自愈：升级既有配置，不能只 append。自定义 wrapper 用 marker 精准识别保护。
- 待办（非阻塞）：`throttled-stop.sh` 用 `/tmp` 状态文件（Windows 无 `/tmp`）；onboarding `dialoguer` 在非 TTY 崩溃。均与本次 PATH 修复无关，列入后续排查。

## 2026-08-02: Session 多来源安全边界与身份修复

### 问题描述
- 交互菜单允许删除 Codex/OMP，会造成外部历史误删。
- OMP Rename 写入 Claude custom-title 记录但 parser 不读取。
- 顶层与子命令重复定义 source，参数位置可能改变查询范围。
- show 只按裸 session ID 查找，跨来源冲突时静默选错。

### 根本原因
- 多来源扩展沿用 String 分派，缺少统一能力矩阵和复合身份。
- mutation handler 与查询 handler 使用不同扫描路径。
- clap global source 与子命令默认 source 重复建模。

### 解决方案
- 引入 SessionSource、SourceCapabilities、SessionIdentity。
- 非 Claude mutation 在菜单、交互 handler、非交互 handler 和底层 delete 入口多层拒绝。
- 所有 session 子命令统一使用 global source。
- 使用 source-aware resolver 处理唯一、未找到和歧义候选。

### 影响范围
- src/handlers/session.rs
- src/main.rs
- src/session_cache.rs tests
- README.md / docs/user-guide.md

### 预防措施
- 新增来源必须先定义 capabilities。
- 内部身份使用 (source, session_id)，不得以裸 ID 作为全局唯一键。
- CLI global 参数不得在子命令重复定义同名默认值。

## 2026-08-02: Cleanup/Restore 写入边界与 public API compatibility shim

### 问题描述
- 交互式 Cleanup 菜单在 Codex/OMP source 下仍可见，可能按同名 Claude project 执行清理。
- Restore 未检查 source，Codex/OMP 请求会先加载同步状态并进入 Claude 文件扫描。
- source-aware handler 改名后，旧 public Rust 签名和原始文件操作 API 不再兼容；原始路径入口也缺少 Claude projects 根目录约束。

### 根本原因
- 交互菜单和 Restore 逻辑没有在所有文件访问前统一应用 source 能力边界。
- 为支持多来源而调整 API 时，缺少 deprecated compatibility shim 和 raw path containment guard。

### 解决方案
- 新增纯 `cleanup_available` 策略和可测试的菜单 option builder；非 Claude source 不构造/显示 Cleanup，dispatch 仍保留防御校验。
- 新增 source-aware Restore handler，第一步拒绝 Codex/OMP；保留旧 Restore 签名并以 `All` 委托。
- 将 Rename/Delete source-aware API 收敛到 `_with_source` 命名，保留旧签名 deprecated shim；main dispatch 使用新 API。
- 恢复 deprecated raw `rename_session`/`delete_session` 签名，并对 canonicalized path 执行 Claude projects 根目录 containment 校验，拒绝外部路径、`..` 父目录逃逸和 symlink escape。
- `delete_session_with_commit` 及其他 Summary-based mutation helper 在任何文件变化前执行同一 root guard；未给既有 source-aware 查询 handler 增加 shim。

### 影响范围
- `src/handlers/session.rs`
- `src/handlers/mod.rs`
- `src/main.rs`
- 仅覆盖 Cleanup/Restore/API compatibility；cache retention 与非 Claude current-project detection 未实现，留待后续 Slice。

### 预防措施
- 所有 mutation/restore 入口先做 source guard，再进行 SyncState、扫描或文件写入。
- public API 重命名必须保留 deprecated shim，并为 raw path API 增加 canonicalized root containment 测试。

## 2026-08-02: 文件日志未接收普通 log 记录

### 问题描述
- 文档宣称 console+file，但普通 log::warn!/debug! 只进入 stderr。
- init/rotation 错误在 main 中被 .ok() 静默吞掉。

### 根本原因
- env_logger 只配置 stderr；log_to_file 是独立手工 helper。
- logger 在 Cli::parse 前初始化，无法消费 --debug/--log-file。

### 解决方案
- 自定义 DualLogger 同时写 stderr/file。
- parse CLI 后初始化；文件失败显式降级，注册失败返回错误。
- 三代轮转、0600 权限、invocation ID 和脱敏。
- 使用与日志同目录的 `ccs.log.lock`（fs4 独占锁）保护跨进程的权限修复、轮转和打开；当前文件、`.1`/`.2`/`.3` 备份及 lock file 在 Unix 均收紧为 0600。
- 扩展脱敏覆盖 token、access/refresh token、password/passwd、secret、client secret、API key、Authorization Bearer/Basic、URL userinfo，并将 CRLF/CR/LF 规范化为字面量 `\\n`。

### 影响范围
- logger.rs、main.rs、Cargo.toml、logger CLI tests、README/user guide。

### 预防措施
- logger sink 行为必须有真实 Record 与 CLI 子进程测试，包括共享日志路径的多进程轮转测试。
- 禁止对 logger 初始化错误无条件 .ok()。
- 日志隐私合同只保证常见 credential、Authorization 和 URL userinfo 自动脱敏；不主动记录会话正文，其他未识别敏感内容仍不应放入 CLI 参数或日志消息。

## 2026-08-02: Session 扫描错误静默导致结果看似成功但不完整

### 问题描述
- `session` 多来源扫描此前可能静默忽略 WalkDir/目录项、metadata/mtime、parser 和 session index cache 错误。
- 用户仍能看到部分结果和成功退出码，却无法判断结果是否不完整，也无法把一次 JSON 查询关联到文件日志中的详细记录。

### 根本原因
- 扫描流程使用忽略错误的遍历/解析路径，单文件失败没有统一计数和继续策略。
- cache 的兼容 `load`/`save` API 只返回空 cache 或吞掉写入失败，调用方没有可消费的状态。
- 文本与 JSON handler 没有共享扫描结果对象，无法统一输出 warning 和 schema。

### 解决方案
- 引入 `SessionScanResult { summaries, diagnostics }`，统一统计 `files_seen`、解析/跳过、malformed、I/O、cache、命中/未命中、字节数和耗时；单项错误记录后继续扫描，缺失 root 仍作为正常空来源。
- cache 保留旧 API，同时提供带状态的加载和显式保存错误接口，失败不丢弃已解析 summaries。
- 文本 handler 只输出一条聚合 degraded warning；overview/search/show JSON 统一附加 `schema_version` 和 `diagnostics`。`diagnostic_id` 复用 logger invocation ID，warning 只保留 path hash 和受控安全摘要，避免泄露完整路径、会话正文或凭据。

### 影响范围
- `src/session_diagnostics.rs`、`src/session_cache.rs`、`src/handlers/session.rs`、`src/logger.rs`、相关集成测试。
- 更新 `README.md` 与 `docs/user-guide.md`，明确 JSON contract、diagnostic ID 关联和当前排除项。
- 明确不包含 `session list --json`、`session doctor`、Hook、cache retention/mtime correctness 或搜索索引。

### 预防措施
- focused tests 必须覆盖 diagnostics、cache 状态、session handler 和隔离的真实 CLI JSON/text degraded 行为；fixture 使用 tempfile HOME/config/session roots，禁止读写真实用户配置或 session。
- 运行 `cargo fmt`、`cargo clippy -- -D warnings`、全量 `cargo test` 和 `git diff --check`；新增 warning 先定位到具体兼容 API，禁止用模块级或 crate 级 broad `allow` 掩盖。
- 既有 `tests/test_onboarding.rs` 的 `temp_dir` unused warning 仅记录，留待独立清理，不混入本 Slice scope。

### Coordinator review correction
- cache 诊断不再记录 cache 原始路径、文件名或底层错误文本；缺失 cache 仅 debug 记录固定摘要，读失败、非法 JSON、版本不匹配分别使用受控类别摘要，兼容 save 失败也只输出无路径 warning。详细诊断仍进入 `ccs::scan_diagnostics` 文件日志并由 `diagnostic_id` 关联。
- Claude projects 根目录 `read_dir` 改为 best-effort：根目录不可读或不是目录时只计一个 I/O degraded warning，Codex/OMP 和其他已取得的 session 结果继续返回，不再因 Claude 根目录错误中止整个扫描。
- 删除 `main.rs` logger 模块级、`LoggerInitStatus` 结构级和 `SessionRoots` 结构级 broad `dead_code` allowance；仅保留 `load`、`save`、`mtime_secs` 兼容 API，以及 logger 初始化/兼容字段所需的 item/field-level allowance，并在代码中注明兼容原因。
- 集成测试改为每个 CLI 子进程使用临时 `--log-file`，移除 `RUST_LOG`，覆盖 corrupt cache、Claude root regular-file、overview/search/show JSON、5 个 degraded text 命令；断言 stdout/stderr/log 均无 HOME/config 路径泄露，且 JSON diagnostic ID 能关联日志 invocation。

### P2 review closure
- 新增 cache status 精确单测：cache path 为目录时 warning 恰为 `cache read failed`；version=999 时 warning 恰为 `cache version mismatch`。
- 新增隔离 CLI JSON 测试：cache path 为目录时 `cache_errors=2`（load read failure + 末尾 save failure），version mismatch 时 `cache_errors=1`（load failure 后 save 成功）；两者均验证 diagnostic ID↔log invocation，stdout/stderr/log 不含 config path、`session_index.json`、原始 OS/JSON 错误，只保留安全 cache detail hash。
- 完成全仓 `#[allow(dead_code)]` 审计。删除已实际调用的 session path guard allowances；session/cache/logger 其余 allowance 均为 item/field 级公开兼容 API，并在 `scan-task-5-report.md` 中逐项列出调用/测试证据。无 crate/module/struct-level broad allowance。
- P2 gates：cache 10+10、session handlers 58+58、CLI integration 6、`cargo test --lib` 334 passed、clippy `-D warnings`、fmt check、diff check 全部通过。

## 2026-08-03: 路径安全与会话缓存身份加固（未 commit/push）

### 问题描述
- project-name-only 模式可将 `.`/`..` 或带分隔符的项目名拼接到 projects 根外；restore 过去复用远端摘要中的 `file_path`，存在远端路径穿越、符号链接和本地目标逃逸风险。
- 三类 session scanner 可能把文件 symlink 当作候选文件；缓存使用原始/损失转换路径时，根目录别名会产生重复身份，非 UTF-8 路径也不应通过 lossy 字符串重新打开。

### 根本原因
- push/delete/restore 缺少统一的 component、relative path 和 root containment 校验；restore 的源、目的路径来源边界混杂。
- 候选校验使用了会跟随 symlink 的 metadata 语义，cache key 没有统一 canonical identity。

### 解决方案
- 新增 `src/path_security.rs`，统一拒绝空、`.`、`..`、绝对路径和 Unix/Windows 分隔符异常；使用 `symlink_metadata`、canonical containment 和 existing-component 检查。
- push、delete、restore 的 project-name-only/relative path 均经共享 helper；restore 注入 remote/local projects root，重新构造本地目标，拒绝远端源和父路径 symlink，并在最终 copy 前复核。
- Claude、Codex、OMP scanner 共享 regular non-symlink candidate 语义；正常路径用 canonical UTF-8 cache key，非 UTF-8 路径只解析不缓存；cache revalidation 不再复用 symlink。

### 影响范围
- `src/path_security.rs`、`src/lib.rs`、`src/main.rs`、`src/handlers/session.rs`、`src/sync/discovery.rs`、`src/sync/push.rs`、`src/session_cache.rs`。
- 新增 tempfile 隔离的 traversal、restore success、file symlink、canonical alias 和 non-UTF-8 回归测试；未触碰真实用户 session/config 数据，未 commit/push/release。

### 验证与限制
- path helper、restore、三来源 symlink scanner、canonical cache identity、cache 和 project traversal focused tests 已通过；全量测试曾暴露并修正既有测试对 canonical key/metadata operation 的旧断言。
- 同一用户可在“复核”与最终 filesystem 操作之间替换目录或文件；当前跨平台实现无法提供 openat/O_NOFOLLOW 风格的原子目录句柄操作，因此仍存在不可完全消除的 TOCTOU 残余。代码在最终操作前重复 symlink/containment 检查，不能宣称竞态完全消除。

### 预防措施
- 后续任何新 session source 或 sync layout 必须复用 `path_security`，不得重新拼接未验证 project/file 路径。
- cache key 变更必须同时检查 v3 兼容、legacy API、root alias、symlink stale entry 和 non-UTF-8 行为；发布前执行 focused、all-target clippy、fmt、full test、release check 与 diff check。

## 2026-08-03：DualLogger 终审发布阻断修复（未 commit/push）

### 问题描述
- DualLogger rereview 发现 rotation commit 失败后若 rollback 自身失败，`TempDir` Drop 可能删除 transaction 中唯一的旧日志数据。
- staging 使用 path-based `std::fs::copy`，source 在 precheck 后被 symlink/reparse 替换时存在跟随边界；live rename 也缺少 opened-handle metadata revalidation。
- hosted `file://` URI path 遇到逗号、引号、`]`、`)` 等合法字符会提前结束 sanitizer 匹配并泄漏尾部。
- 文档未清楚区分遵守 `.lock` 的 ccs 协作进程与不使用 advisory lock 的外部 rotator。

### 根本原因
- rollback failure 分支直接返回，transaction 仍由 `TempDir` 管理；path copy 只依赖先前的 `symlink_metadata` 检查；URI 正则把合法 URI path punctuation 当作边界。

### 解决方案
- rollback 自身失败时显式 `mem::forget(transaction)`，返回固定、可见、path-free 的安全错误；新增 rollback cleanup failpoint/transaction retention 回归。
- rotation staging 改为从已打开 no-follow source handle 复制到 no-follow/create-new staging 文件，复制后校验长度；live target rename 前使用 opened handle 与 inode/metadata 复核。非 Linux/macOS Unix 分支不再关闭 no-follow flag。
- hosted `file://` sanitizer 消费完整到空白字符的 URI path，并加入合法 punctuation exact cases；URI、quoted credential 和 multiline/control 回归均使用 tempfile/纯内存输入。
- README、用户指南和最终修复报告明确 advisory lock 不约束未取得锁的系统 logrotate、用户脚本或其他外部 rotator，不虚假承诺原子协调。

### 影响范围
- `src/logger.rs`、logger/CLI 回归测试、README、用户指南、最终修复报告和本记录。

### 验证与限制
- logger 36、session diagnostics 12、logger CLI 8 focused tests 通过；`cargo test --all-targets -- --test-threads=1` 全部 0 failed。
- `cargo check --all-targets`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`cargo check --release`、`git diff --check` 通过。
- 本机只有 `aarch64-apple-darwin` target；Windows target 未安装，`cargo check --target x86_64-pc-windows-gnu` 因缺少 `core/std` 失败，不能声称已交叉编译。Windows cfg 仅完成静态代码检查和现有平台 flag 单测代码覆盖。
- 仍存在未取得 advisory lock 的外部 rotator 协作边界，以及标准库 path rename 无法完全消除的最后极窄 TOCTOU 窗口；报告已明确不宣称完全消除。

## 2026-08-03：Task 87 扫描诊断错误边界（未 commit/push）

### 问题描述
- scanner 将 Claude/Codex/OMP parser 的系统 I/O 失败与 malformed data 混为 Data warning，可能错误生成 cache removal；cache revalidation error 被静默吞掉；interactive session 刷新路径丢弃 diagnostics。
- warning 只有 category，无法稳定区分 permission denied、not found、invalid data、read failure、changed-during-read 等受控原因。

### 根本原因
- parser `Err` 分支没有统一消费仍持有的 `anyhow::Error` chain；`BufRead::lines()` 的 invalid UTF-8 与 I/O 错误需要按不同业务语义处理。
- cache merge 只返回兼容的 `Result<()>`，没有将单 entry revalidation failure 作为 advisory report 暴露给 scanner。
- interactive handler 使用 summary-only wrapper，刷新时没有消费 `SessionScanResult.diagnostics`。

### 解决方案
- 新增可序列化的 `ScanWarningErrorKind`，从 `anyhow::Error` chain 中的 `std::io::ErrorKind` 或 typed `ChangedDuringRead` marker 推导；warning/log 只保留受控 kind、path hash、detail hash 和可选 line/column，不恢复 raw error、路径或正文。
- Claude/Codex/OMP parser 统一使用 `handle_parser_error`：系统 I/O 标记 source incomplete、记录 Io warning 且不生成 `CacheRemoval`；InvalidData/未知 malformed data 保持 Data warning 和既有 eviction 语义。测试 seam 仅在 `cfg(test)` 下删除 fingerprint 后的文件，覆盖真实 NotFound parser open 路径；Claude parser 另抽出内部 reader seam，真实覆盖 `BufRead::lines()` read-side `UnexpectedEof`。
- scanner/root/WalkDir/metadata/fingerprint/read_dir/history 仍持有的真实 I/O error chain 统一交给 `record_warning_from_error` 或 legacy controlled helper；无底层 error chain 的 non-regular/untrusted 状态显式为 `unknown`，不固定伪报 `read_failed`。
- 新增 `CacheMergeReport`/`CacheRevalidationIssue` 与 `merge_scan_with_report`，保留 `merge_scan_with_result` compatibility wrapper。confirmed missing 仍 prune，stale mismatch 静默保留，revalidation error 记录受控 issue 并保留 entry；scanner 将 issue 转为 Cache warning，业务 summaries 继续返回。
- interactive 初始扫描及 rename/delete/search/cleanup/switch-project 刷新统一通过 report-aware helper，每次 scan 恰好消费一次 warning；clean report 不输出 warning，degraded report 输出一条含 diagnostic ID 的聚合 warning。

### 影响范围
- `src/session_diagnostics.rs`、`src/session_cache.rs`、`src/handlers/session.rs`、三来源 parser 相关扫描代码及 `tests/session_scan_diagnostics_tests.rs`。
- 未升级既有 JSON `schema_version`，未扩展 Task 88 的普通 log callsite 隐私范围，未访问真实用户目录，未 commit/push/release。

### TDD 与验证
- RED：先加入 controlled kind、cache report、interactive writer、三来源 parser I/O 和 cache revalidation regression tests；在生产接口尚不存在时观察到预期缺失符号/编译失败。
- GREEN：focused diagnostics 13、cache 37、session handler 92、Claude parser 48、Codex parser 3、OMP parser 10、CLI diagnostics integration 7 全部通过；随后 `cargo check --all-targets`、host Clippy、Windows GNU target check/Clippy、fmt check、全量串行测试和 `git diff --check` 全部通过。
- 全量 `cargo test --all-targets -- --test-threads=1`：各测试二进制均 0 failed，合计 1040 passed，1 ignored（分组结果保留在 Task 87 report）。

### 预防措施
- 新增 session source 的 parser 错误必须复用统一 helper，并区分系统 I/O 与 confirmed malformed data；任何 cache eviction 必须以 confirmed missing 或 confirmed malformed/partial data 为依据。
- cache merge 新增 advisory 失败时必须提供 report-aware API，同时保留旧 wrapper；interactive 所有刷新必须消费 diagnostics。
- 发布前执行 focused tests、all-targets check/clippy、Windows cross-target check/clippy、fmt、full test 和 diff check；测试始终使用 tempfile/注入 roots/config，禁止真实用户目录。

### 2026-08-03 review remediation
- I1 修复：metadata、fingerprint、WalkDir、root/read_dir、legacy discovery、Codex history 和 current-project discovery 的真实错误链均按 `std::io::ErrorKind`/WalkDir source 受控分类；无 error chain 的状态使用显式 `unknown`，不固定为 `read_failed`。
- I2 修复：`current_file_state` 将 confirmed `NotFound` 与 symlink/non-regular/untrusted object 分离；后者产生 cache revalidation issue 并保留 entry，不得 eviction/prune。新增 symlink 和 directory regression tests。
- M1 加固：Claude parser 增加内部 `from_reader_with_report` seam，使用 failing reader 验证真实 read-side `UnexpectedEof` 仍在 anyhow chain 中；fingerprint 也增加 cfg(test) error seam 验证 `permission_denied`。
- remediation focused、全量、本机+Windows check/clippy、fmt 和 diff check 全部通过；未 commit/push/release。
