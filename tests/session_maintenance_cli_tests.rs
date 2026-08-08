use serde_json::{json, Value};
use serial_test::serial;
use std::fs;
use std::process::{Command, Output};
use tempfile::TempDir;

struct Fixture {
    home: TempDir,
    config: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            home: tempfile::tempdir().expect("home tempdir"),
            config: tempfile::tempdir().expect("config tempdir"),
        };
        fixture.write_sessions();
        fixture
    }

    fn empty() -> Self {
        Self {
            home: tempfile::tempdir().expect("home tempdir"),
            config: tempfile::tempdir().expect("config tempdir"),
        }
    }

    fn write_sessions(&self) {
        let claude = self.home.path().join(".claude/projects/-tmp-task8-project");
        fs::create_dir_all(&claude).expect("Claude project root");
        fs::write(
            claude.join("shared.jsonl"),
            concat!(
                r#"{"type":"user","sessionId":"shared","cwd":"/tmp/task8-project","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"needle claude"}}"#,
                "\n",
                r#"{"type":"assistant","sessionId":"shared","cwd":"/tmp/task8-project","timestamp":"2026-08-02T00:00:01Z","message":{"role":"assistant","content":"answer"}}"#,
                "\n",
            ),
        )
        .expect("Claude session");

        let codex = self.home.path().join(".codex/sessions/2026");
        fs::create_dir_all(&codex).expect("Codex session root");
        fs::write(
            codex.join("shared.jsonl"),
            concat!(
                r#"{"timestamp":"2026-08-02T00:00:00Z","type":"session_meta","payload":{"id":"shared","cwd":"/tmp/task8-project"}}"#,
                "\n",
                r#"{"timestamp":"2026-08-02T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"needle codex"}]}}"#,
                "\n",
            ),
        )
        .expect("Codex session");

        let omp = self.home.path().join(".omp/agent/sessions");
        fs::create_dir_all(&omp).expect("OMP session root");
        fs::write(
            omp.join("shared.jsonl"),
            concat!(
                r#"{"type":"session","version":3,"id":"shared","timestamp":"2026-08-02T00:00:00Z","cwd":"/tmp/task8-project","title":"OMP shared"}"#,
                "\n",
                r#"{"type":"message","timestamp":"2026-08-02T00:00:01Z","message":{"role":"user","content":[{"type":"text","text":"needle omp"}]}}"#,
                "\n",
            ),
        )
        .expect("OMP session");
    }

    fn upsert_state(&self, source: &str, id: &str, entry: Value) {
        let path = self.config.path().join("session-maintenance.json");
        let mut state = if path.exists() {
            serde_json::from_slice::<Value>(&fs::read(&path).expect("read state"))
                .expect("parse state")
        } else {
            json!({"version": 1, "entries": {}, "pending": null})
        };
        state["entries"][format!("{source}:{id}")] = entry;
        fs::write(
            path,
            serde_json::to_vec_pretty(&state).expect("serialize state"),
        )
        .expect("write state");
    }

    fn source_root(&self, source: &str) -> std::path::PathBuf {
        match source {
            "claude" => self.home.path().join(".claude/projects"),
            "codex" => self.home.path().join(".codex/sessions"),
            "omp" => self.home.path().join(".omp/agent/sessions"),
            _ => panic!("unknown source"),
        }
    }

    fn write_recycled(
        &self,
        source: &str,
        id: &str,
        original_relative_path: &str,
        project: &str,
        content: &str,
    ) -> std::path::PathBuf {
        let source_root = self.source_root(source);
        let source_path = source_root.join(original_relative_path);
        fs::create_dir_all(source_path.parent().expect("source parent")).expect("source parent");
        fs::create_dir_all(self.config.path().join("session-recycle")).expect("recycle root");
        let fingerprint = blake3::hash(content.as_bytes()).to_hex().to_string();
        let final_path = self
            .config
            .path()
            .join("session-recycle")
            .join(source)
            .join(format!("id-{}", blake3::hash(id.as_bytes()).to_hex()))
            .join(format!("{fingerprint}.jsonl"));
        fs::create_dir_all(final_path.parent().expect("recycle parent")).expect("recycle parent");
        fs::write(&final_path, content).expect("recycled content");
        let entry = json!({
            "identity": {"source": source, "session_id": id},
            "original_relative_path": original_relative_path,
            "project_name": project,
            "fingerprint": fingerprint,
            "lifecycle": "recycled",
            "classifier_version": 1,
            "score": 100,
            "reason_codes": [],
            "hidden_since": null,
            "recycled_at": "2026-08-08T12:00:00Z",
            "purged_at": null,
            "keep": false,
            "explicit_test": true
        });
        self.upsert_state(source, id, entry);
        final_path
    }

    fn write_hidden(
        &self,
        source: &str,
        id: &str,
        original_relative_path: &str,
        project: &str,
        content: &str,
    ) -> std::path::PathBuf {
        let source_root = self.source_root(source);
        let source_path = source_root.join(original_relative_path);
        fs::create_dir_all(source_path.parent().expect("source parent")).expect("source parent");
        fs::write(&source_path, content).expect("hidden content");
        let fingerprint = blake3::hash(content.as_bytes()).to_hex().to_string();
        let entry = json!({
            "identity": {"source": source, "session_id": id},
            "original_relative_path": original_relative_path,
            "project_name": project,
            "fingerprint": fingerprint,
            "lifecycle": "hidden",
            "classifier_version": 1,
            "score": 100,
            "reason_codes": ["explicit_test_marker"],
            "hidden_since": "2026-08-01T12:00:00Z",
            "recycled_at": null,
            "purged_at": null,
            "keep": false,
            "explicit_test": true
        });
        self.upsert_state(source, id, entry);
        source_path
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ccs"))
            .args(args)
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("CLAUDE_CODE_SYNC_CONFIG_DIR", self.config.path())
            .env_remove("RUST_LOG")
            .output()
            .expect("run ccs")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
#[serial]
fn search_json_and_show_json_include_visibility_and_schema() {
    let fixture = Fixture::new();
    let search = fixture.run(&["session", "search", "needle", "--json"]);
    assert!(search.status.success(), "{}", stderr(&search));
    let payload: Value = serde_json::from_str(&stdout(&search)).expect("search JSON");
    assert_eq!(payload["schema_version"], 1);
    let results = payload["session_results"]
        .as_array()
        .expect("session results");
    assert_eq!(results.len(), 3);
    assert!(results
        .iter()
        .all(|result| result["visibility"] == "visible"));

    let show = fixture.run(&[
        "session", "show", "shared", "--source", "codex", "--json", "--head", "1",
    ]);
    assert!(show.status.success(), "{}", stderr(&show));
    let payload: Value = serde_json::from_str(&stdout(&show)).expect("show JSON");
    assert_eq!(payload["schema_version"], 1);
    assert_eq!(payload["visibility"], "visible");
    assert_eq!(payload["source"], "codex");
}

#[test]
#[serial]
fn source_qualified_show_resolves_ambiguous_ids_and_text_search_has_source_rows() {
    let fixture = Fixture::new();
    let ambiguous = fixture.run(&["session", "show", "shared", "--head", "1"]);
    assert!(!ambiguous.status.success());
    assert!(stderr(&ambiguous).contains("Ambiguous session ID 'shared'"));

    let text = fixture.run(&["session", "search", "needle", "--source", "omp"]);
    assert!(text.status.success(), "{}", stderr(&text));
    assert!(stdout(&text).contains("[OM]"), "{}", stdout(&text));
}

#[test]
#[serial]
fn recycled_sessions_appear_in_search_show_and_include_hidden_list_only() {
    let fixture = Fixture::empty();
    let recycled_content = concat!(
        r#"{"type":"user","sessionId":"recycled","cwd":"/tmp/recycled-project","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"recycled-keyword"}}"#,
        "\n",
        r#"{"type":"assistant","sessionId":"recycled","cwd":"/tmp/recycled-project","timestamp":"2026-08-02T00:00:01Z","message":{"role":"assistant","content":"answer"}}"#,
        "\n",
    );
    fixture.write_recycled(
        "claude",
        "recycled",
        "-tmp-recycled-project/recycled.jsonl",
        "recycled-project",
        recycled_content,
    );
    let hidden_content = concat!(
        r#"{"type":"user","sessionId":"hidden","cwd":"/tmp/hidden-project","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"hidden-keyword"}}"#,
        "\n",
    );
    fixture.write_hidden(
        "claude",
        "hidden",
        "-tmp-hidden-project/hidden.jsonl",
        "hidden-project",
        hidden_content,
    );

    let default_list = fixture.run(&["session", "list"]);
    assert!(default_list.status.success(), "{}", stderr(&default_list));
    assert!(!stdout(&default_list).contains("recycled"));
    assert!(!stdout(&default_list).contains("hidden"));

    let hidden_list = fixture.run(&["session", "list", "--include-hidden"]);
    assert!(hidden_list.status.success(), "{}", stderr(&hidden_list));
    assert!(stdout(&hidden_list).contains("[recycled]"));
    assert!(stdout(&hidden_list).contains("[hidden]"));

    let search = fixture.run(&["session", "search", "recycled-keyword"]);
    assert!(search.status.success(), "{}", stderr(&search));
    assert!(stdout(&search).contains("[recycled]"));

    let search_json = fixture.run(&["session", "search", "recycled-keyword", "--json"]);
    assert!(search_json.status.success(), "{}", stderr(&search_json));
    let payload: Value = serde_json::from_str(&stdout(&search_json)).expect("search JSON");
    assert_eq!(payload["session_results"][0]["visibility"], "recycled");

    let active_only = fixture.run(&[
        "session",
        "search",
        "recycled-keyword",
        "--active-only",
        "--json",
    ]);
    assert!(active_only.status.success(), "{}", stderr(&active_only));
    let payload: Value = serde_json::from_str(&stdout(&active_only)).expect("active-only JSON");
    assert_eq!(payload["session_results"].as_array().unwrap().len(), 0);

    let show_json = fixture.run(&[
        "session", "show", "recycled", "--source", "claude", "--json", "--head", "1",
    ]);
    assert!(show_json.status.success(), "{}", stderr(&show_json));
    let payload: Value = serde_json::from_str(&stdout(&show_json)).expect("show JSON");
    assert_eq!(payload["visibility"], "recycled");

    let show_text = fixture.run(&[
        "session", "show", "recycled", "--source", "claude", "--head", "1",
    ]);
    assert!(show_text.status.success(), "{}", stderr(&show_text));
    assert!(stdout(&show_text).contains("[recycled]"));
}

#[test]
#[serial]
fn specific_restore_moves_recycled_copy_back_for_all_sources_and_keeps_it() {
    let fixtures = [
        (
            "claude",
            "cc-restore",
            "-tmp-restore-project/cc-restore.jsonl",
            concat!(
                r#"{"type":"user","sessionId":"cc-restore","cwd":"/tmp/restore-project","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"restore claude"}}"#,
                "\n",
            ),
        ),
        (
            "codex",
            "cx-restore",
            "2026/cx-restore.jsonl",
            concat!(
                r#"{"type":"session_meta","payload":{"id":"cx-restore","cwd":"/tmp/restore-project"},"timestamp":"2026-08-02T00:00:00Z"}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"restore codex"}]},"timestamp":"2026-08-02T00:00:01Z"}"#,
                "\n",
            ),
        ),
        (
            "omp",
            "om-restore",
            "om-restore.jsonl",
            concat!(
                r#"{"type":"session","version":3,"id":"om-restore","timestamp":"2026-08-02T00:00:00Z","cwd":"/tmp/restore-project","title":"OMP restore"}"#,
                "\n",
                r#"{"type":"message","timestamp":"2026-08-02T00:00:01Z","message":{"role":"user","content":[{"type":"text","text":"restore omp"}]}}"#,
                "\n",
            ),
        ),
    ];

    for (source, id, relative, content) in fixtures {
        let fixture = Fixture::empty();
        let final_path = fixture.write_recycled(source, id, relative, "restore-project", content);
        let probe = fixture.run(&["session", "search", "restore", "--source", source, "--json"]);
        assert!(probe.status.success(), "{}", stderr(&probe));
        let probe_json: Value = serde_json::from_str(&stdout(&probe)).expect("probe JSON");
        assert_eq!(
            probe_json["session_results"].as_array().unwrap().len(),
            1,
            "{probe_json}"
        );
        let output = fixture.run(&["session", "restore", id, "--source", source]);
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(
            !final_path.exists(),
            "recycle file remains: {}",
            final_path.display()
        );
        assert!(fixture.source_root(source).join(relative).exists());
        let state: Value = serde_json::from_slice(
            &fs::read(fixture.config.path().join("session-maintenance.json")).unwrap(),
        )
        .unwrap();
        let entry = &state["entries"][format!("{source}:{id}")];
        assert_eq!(entry["lifecycle"], "visible");
        assert_eq!(entry["keep"], true);
    }
}

#[test]
#[serial]
fn include_hidden_help_describes_hidden_and_recycled_only() {
    let fixture = Fixture::new();
    let output = fixture.run(&["session", "--help"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let help = stdout(&output);
    assert!(
        help.contains("hidden") && help.contains("recycled"),
        "{help}"
    );
    assert!(!help.contains("purged"), "{help}");
}

#[test]
#[serial]
fn restore_without_recycled_copy_reports_exact_non_claude_error() {
    let fixture = Fixture::new();
    for (source, label) in [("codex", "CX"), ("omp", "OM")] {
        let output = fixture.run(&["session", "restore", "shared", "--source", source]);
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains(&format!(
                "No local recycled copy is available for {label} session shared"
            )),
            "{}",
            stderr(&output)
        );
    }
}

#[test]
#[serial]
fn source_all_does_not_fallback_to_claude_for_missing_codex_or_omp_recycled_copy() {
    for (source, id, label, relative, content) in [
        (
            "codex",
            "same-id",
            "CX",
            "2026/same-id.jsonl",
            concat!(
                r#"{"type":"session_meta","payload":{"id":"same-id","cwd":"/tmp/project"},"timestamp":"2026-08-02T00:00:00Z"}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"codex copy"}]},"timestamp":"2026-08-02T00:00:01Z"}"#,
                "\n",
            ),
        ),
        (
            "omp",
            "same-id",
            "OM",
            "same-id.jsonl",
            concat!(
                r#"{"type":"session","version":3,"id":"same-id","timestamp":"2026-08-02T00:00:00Z","cwd":"/tmp/project","title":"OMP copy"}"#,
                "\n",
                r#"{"type":"message","timestamp":"2026-08-02T00:00:01Z","message":{"role":"user","content":[{"type":"text","text":"omp copy"}]}}"#,
                "\n",
            ),
        ),
    ] {
        let fixture = Fixture::empty();
        let recycled = fixture.write_recycled(source, id, relative, "project", content);
        fs::remove_file(recycled).expect("remove recycled copy");

        let output = fixture.run(&["session", "restore", id, "--source", "all"]);
        assert!(
            !output.status.success(),
            "unexpected success: {}",
            stdout(&output)
        );
        assert!(
            stderr(&output).contains(&format!(
                "No local recycled copy is available for {label} session {id}"
            )),
            "{}",
            stderr(&output)
        );
    }
}

#[test]
#[serial]
fn hidden_state_only_restore_fails_without_changing_state() {
    let fixture = Fixture::empty();
    let source_path = fixture.write_hidden(
        "claude",
        "hidden-state-only",
        "-tmp-hidden-project/hidden-state-only.jsonl",
        "hidden-project",
        concat!(
            r#"{"type":"user","sessionId":"hidden-state-only","cwd":"/tmp/hidden-project","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"hidden"}}"#,
            "\n",
        ),
    );
    fs::remove_file(source_path).expect("remove hidden local copy");

    let output = fixture.run(&[
        "session",
        "restore",
        "hidden-state-only",
        "--source",
        "claude",
    ]);
    assert!(
        !output.status.success(),
        "unexpected success: {}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("no scanned local summary"),
        "{}",
        stderr(&output)
    );
    let state: Value = serde_json::from_slice(
        &fs::read(fixture.config.path().join("session-maintenance.json")).unwrap(),
    )
    .unwrap();
    let entry = &state["entries"]["claude:hidden-state-only"];
    assert_eq!(entry["lifecycle"], "hidden");
    assert_eq!(entry["keep"], false);
}

#[test]
#[serial]
fn malformed_recycled_entry_is_skipped_without_breaking_query() {
    let fixture = Fixture::empty();
    fixture.write_recycled(
        "claude",
        "malformed",
        "-tmp-project/malformed.jsonl",
        "project",
        "not-json\\n",
    );

    let output = fixture.run(&[
        "session", "search", "not-json", "--source", "claude", "--json",
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("search JSON");
    assert!(payload["session_results"].as_array().unwrap().is_empty());
}

#[test]
#[serial]
fn fingerprint_mismatch_recycled_entry_is_skipped_without_following_content() {
    let fixture = Fixture::empty();
    let final_path = fixture.write_recycled(
        "claude",
        "fingerprint-mismatch",
        "-tmp-project/fingerprint-mismatch.jsonl",
        "project",
        concat!(
            r#"{"type":"user","sessionId":"fingerprint-mismatch","cwd":"/tmp/project","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"original-keyword"}}"#,
            "\n",
        ),
    );
    fs::write(
        final_path,
        concat!(
            r#"{"type":"user","sessionId":"fingerprint-mismatch","cwd":"/tmp/project","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"changed-keyword"}}"#,
            "\n",
        ),
    )
    .expect("rewrite recycled content");

    let output = fixture.run(&[
        "session",
        "search",
        "changed-keyword",
        "--source",
        "claude",
        "--json",
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("search JSON");
    assert!(payload["session_results"].as_array().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
#[serial]
fn symlink_recycled_entry_is_skipped_without_following_external_file() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::empty();
    let external = fixture.home.path().join("external.jsonl");
    fs::write(
        &external,
        concat!(
            r#"{"type":"user","sessionId":"symlinked","cwd":"/tmp/project","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"external-secret"}}"#,
            "\n",
        ),
    )
    .expect("external content");
    let final_path = fixture.write_recycled(
        "claude",
        "symlinked",
        "-tmp-project/symlinked.jsonl",
        "project",
        concat!(
            r#"{"type":"user","sessionId":"symlinked","cwd":"/tmp/project","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"recycled"}}"#,
            "\n",
        ),
    );
    fs::remove_file(&final_path).expect("remove recycled regular file");
    symlink(&external, &final_path).expect("symlink recycled path");

    let output = fixture.run(&[
        "session",
        "search",
        "external-secret",
        "--source",
        "claude",
        "--json",
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("search JSON");
    assert!(payload["session_results"].as_array().unwrap().is_empty());
}

#[test]
#[serial]
fn degraded_restore_scan_fails_safe_without_changing_hidden_state() {
    let fixture = Fixture::empty();
    fixture.write_hidden(
        "claude",
        "degraded-hidden",
        "-tmp-project/degraded-hidden.jsonl",
        "project",
        concat!(
            r#"{"type":"user","sessionId":"degraded-hidden","cwd":"/tmp/project","timestamp":"2026-08-02T00:00:00Z","message":{"role":"user","content":"hidden"}}"#,
            "\n",
        ),
    );
    let broken = fixture
        .source_root("claude")
        .join("-tmp-project/broken.jsonl");
    fs::write(broken, "not-json\\n").expect("malformed active session");

    let output = fixture.run(&[
        "session",
        "restore",
        "degraded-hidden",
        "--source",
        "claude",
    ]);
    assert!(
        !output.status.success(),
        "unexpected success: {}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("source scan was incomplete"),
        "{}",
        stderr(&output)
    );
    let state: Value = serde_json::from_slice(
        &fs::read(fixture.config.path().join("session-maintenance.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        state["entries"]["claude:degraded-hidden"]["lifecycle"],
        "hidden"
    );
}
