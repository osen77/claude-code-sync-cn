use serde_json::Value;
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
        let home = tempfile::tempdir().expect("home tempdir");
        let config = tempfile::tempdir().expect("config tempdir");
        let fixture = Self { home, config };
        fixture.write_sessions();
        fixture
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
