//! Integration tests for the `synaxis` binary.
//!
//! HOME and XDG_* are pointed at a unique tempdir so the tests never read
//! the real home directory. Host tools (`bunx`) are faked on PATH; no
//! network is used.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn unique_temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "synaxis-it-{tag}-{}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("tempdir");
    dir
}

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_synaxis"))
}

fn write_skill(source: &Path, name: &str, body: &str) {
    let dir = source.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), body).unwrap();
}

fn default_skill_body() -> &'static str {
    "---\nname: test\n---\nbody\n"
}

fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
    }
    path
}

fn run_in_home(home: &Path, args: &[&str]) -> Output {
    run_in_home_with_path(home, args, None)
}

fn run_in_home_with_path(home: &Path, args: &[&str], extra_path: Option<&Path>) -> Output {
    let mut cmd = bin();
    cmd.args(args);
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
    cmd.env("XDG_DATA_HOME", home.join(".local/share"));
    if let Some(prefix) = extra_path {
        let rest = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{rest}", prefix.display()));
    }
    cmd.output().expect("run synaxis")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn write_config(home: &Path, body: &str) {
    let path = home.join(".config/synaxis/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

#[test]
fn help_exits_zero_and_prints_usage() {
    let home = unique_temp("help");
    for flag in ["--help", "-h"] {
        let out = run_in_home(&home, &[flag]);
        assert!(
            out.status.success(),
            "{flag} should exit 0: {}",
            stderr(&out)
        );
        let text = stdout(&out);
        assert!(text.contains("Usage: synaxis"), "{flag} stdout: {text}");
        assert!(text.contains("--full"), "{flag} stdout: {text}");
        assert!(text.contains("--check"), "{flag} stdout: {text}");
        assert!(text.contains("--init"), "{flag} stdout: {text}");
    }
    fs::remove_dir_all(&home).ok();
}

#[test]
fn init_writes_default_config_and_is_idempotent() {
    let home = unique_temp("init");
    let cfg = home.join(".config/synaxis/config.toml");

    let first = run_in_home(&home, &["--init"]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(stdout(&first).contains("Wrote default config"));
    assert!(cfg.is_file());
    let written = fs::read_to_string(&cfg).unwrap();
    assert!(written.contains("[skills]"));
    assert!(written.contains("[mcp]"));
    assert!(written.contains("[ce]"));
    assert!(written.contains("~/skills"));

    fs::write(&cfg, "edited-by-test\n").unwrap();
    let second = run_in_home(&home, &["--init"]);
    assert!(second.status.success(), "{}", stderr(&second));
    assert!(stdout(&second).contains("Config already exists"));
    assert_eq!(fs::read_to_string(&cfg).unwrap(), "edited-by-test\n");
    fs::remove_dir_all(&home).ok();
}

#[test]
fn init_fails_when_config_parent_cannot_be_created() {
    let home = unique_temp("init-fail");
    // `.config` as a file blocks `~/.config/synaxis/`.
    fs::write(home.join(".config"), "not a directory").unwrap();
    let out = run_in_home(&home, &["--init"]);
    assert!(!out.status.success(), "expected non-zero exit");
    assert_ne!(
        out.status.code(),
        Some(0),
        "must not succeed when create_dir_all fails"
    );
    fs::remove_dir_all(&home).ok();
}

#[test]
fn check_lists_skills_without_writing_targets() {
    let home = unique_temp("check");
    let source = home.join("receptors");
    fs::create_dir_all(&source).unwrap();
    write_skill(&source, "alpha", default_skill_body());
    write_skill(&source, "beta", "no frontmatter\n");
    let target = home.join("out");
    write_config(
        &home,
        &format!("[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = []\n"),
    );

    let out = run_in_home(&home, &["--check"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("Skills in"), "{text}");
    assert!(text.contains("alpha"), "{text}");
    assert!(text.contains("beta"), "{text}");
    assert!(text.contains("no frontmatter"), "{text}");
    assert!(text.contains("2 skills"), "{text}");
    assert!(!target.exists(), "--check must not create targets");
    fs::remove_dir_all(&home).ok();
}

#[test]
fn default_paths_sync_without_config_file() {
    let home = unique_temp("defaults");
    let source = home.join("skills");
    fs::create_dir_all(&source).unwrap();
    write_skill(&source, "alpha", default_skill_body());

    let out = run_in_home(&home, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    for rel in [
        ".claude/skills",
        ".opencode/skills",
        ".codex/skills",
        ".agents/skills",
    ] {
        let dest = home.join(rel).join("alpha");
        assert!(dest.is_symlink(), "{rel} should receive the skill");
        assert_eq!(fs::read_link(&dest).unwrap(), source.join("alpha"));
    }
    fs::remove_dir_all(&home).ok();
}

#[test]
fn default_sync_links_skills_into_configured_target() {
    let home = unique_temp("sync");
    let source = home.join("receptors");
    fs::create_dir_all(&source).unwrap();
    write_skill(&source, "alpha", default_skill_body());
    let target = home.join("out");
    write_config(
        &home,
        &format!("[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = []\n"),
    );

    let out = run_in_home(&home, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("Skills: 1 synced"),
        "stderr: {}",
        stderr(&out)
    );
    let dest = target.join("alpha");
    assert!(dest.is_symlink());
    assert_eq!(fs::read_link(&dest).unwrap(), source.join("alpha"));
    fs::remove_dir_all(&home).ok();
}

#[test]
fn full_sync_writes_mcp_and_invokes_fake_bunx() {
    let home = unique_temp("full");
    let source = home.join("receptors");
    fs::create_dir_all(&source).unwrap();
    write_skill(&source, "alpha", default_skill_body());
    let skills_target = home.join("out");
    let mcp_src = home.join("mcp-servers.json");
    fs::write(
        &mcp_src,
        r#"{
            "mcpServers": { "fs": { "command": "npx" } },
            "_codexExtras": { "c7": { "url": "https://example.com" } }
        }"#,
    )
    .unwrap();
    let oc = home.join("opencode/mcp.json");
    let cx = home.join("codex/config.toml");
    let ce = home.join("marketplace");
    fs::create_dir_all(&ce).unwrap();
    let marker = home.join("bunx-ran");
    let fake_bin = home.join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    write_executable(
        &fake_bin,
        "bunx",
        &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" > {marker:?}\nexit 0\n"),
    );
    write_config(
        &home,
        &format!(
            "[skills]\nsource = {source:?}\ntargets = [{skills_target:?}]\nskip = []\n\n\
             [mcp]\nsource = {mcp_src:?}\nopencode = {oc:?}\ncodex = {cx:?}\n\n\
             [ce]\nsource = {ce:?}\nplatforms = [\"codex\"]\n"
        ),
    );

    let out = run_in_home_with_path(&home, &["--full"], Some(&fake_bin));
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(skills_target.join("alpha").is_symlink());
    let oc_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&oc).unwrap()).unwrap();
    assert_eq!(oc_json["mcpServers"]["fs"]["command"], "npx");
    let cx_text = fs::read_to_string(&cx).unwrap();
    assert!(cx_text.contains("# synaxis-mcp-begin"));
    assert!(cx_text.contains("[mcp_servers.c7]"));
    assert!(marker.is_file(), "fake bunx should have run");
    let bunx_args = fs::read_to_string(&marker).unwrap();
    assert!(bunx_args.contains("@every-env/compound-plugin"));
    assert!(bunx_args.contains("--to"));
    assert!(bunx_args.contains("codex"));
    let combined = format!("{}{}", stdout(&out), stderr(&out));
    assert!(combined.contains("wrote OpenCode config"), "{combined}");
    assert!(combined.contains("wrote Codex config block"), "{combined}");
    assert!(
        combined.contains("compound-engineering installed to codex"),
        "{combined}"
    );
    assert!(combined.contains("Done. Restart"), "{combined}");
    fs::remove_dir_all(&home).ok();
}

#[test]
fn full_skips_ce_when_marketplace_is_absent() {
    let home = unique_temp("full-no-ce");
    let source = home.join("receptors");
    fs::create_dir_all(&source).unwrap();
    write_skill(&source, "alpha", default_skill_body());
    let skills_target = home.join("out");
    let mcp_src = home.join("mcp-servers.json");
    fs::write(&mcp_src, r#"{"mcpServers":{}}"#).unwrap();
    let oc = home.join("opencode/mcp.json");
    let cx = home.join("codex/config.toml");
    write_config(
        &home,
        &format!(
            "[skills]\nsource = {source:?}\ntargets = [{skills_target:?}]\nskip = []\n\n\
             [mcp]\nsource = {mcp_src:?}\nopencode = {oc:?}\ncodex = {cx:?}\n\n\
             [ce]\nsource = {:?}\n",
            home.join("no-such-marketplace")
        ),
    );

    let out = run_in_home(&home, &["--full"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("every-marketplace not installed")
            || stdout(&out).contains("every-marketplace not installed")
    );
    fs::remove_dir_all(&home).ok();
}

/// Expected: `--full` should exit non-zero when a CE install fails, so a git
/// hook cannot report success after a broken compound-engineering install.
/// Observed: bunx failure is warned and the process still exits 0.
#[test]
fn documents_defect_full_exits_zero_when_ce_fails() {
    let home = unique_temp("ce-fail");
    let source = home.join("receptors");
    fs::create_dir_all(&source).unwrap();
    write_skill(&source, "alpha", default_skill_body());
    let skills_target = home.join("out");
    let mcp_src = home.join("mcp-servers.json");
    fs::write(&mcp_src, r#"{"mcpServers":{}}"#).unwrap();
    let oc = home.join("opencode/mcp.json");
    let cx = home.join("codex/config.toml");
    let ce = home.join("marketplace");
    fs::create_dir_all(&ce).unwrap();
    let fake_bin = home.join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    write_executable(&fake_bin, "bunx", "#!/bin/sh\nexit 1\n");
    write_config(
        &home,
        &format!(
            "[skills]\nsource = {source:?}\ntargets = [{skills_target:?}]\nskip = []\n\n\
             [mcp]\nsource = {mcp_src:?}\nopencode = {oc:?}\ncodex = {cx:?}\n\n\
             [ce]\nsource = {ce:?}\nplatforms = [\"codex\"]\n"
        ),
    );

    let out = run_in_home_with_path(&home, &["--full"], Some(&fake_bin));
    assert!(
        out.status.success(),
        "current behaviour: --full exits 0 after bunx failure; stderr: {}",
        stderr(&out)
    );
    let combined = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        combined.contains("install failed"),
        "expected a warning: {combined}"
    );
    fs::remove_dir_all(&home).ok();
}

/// Expected: a missing skills source is a non-zero graceful error. Observed:
/// the binary panics (`cannot read skills source`).
#[test]
fn documents_defect_missing_source_is_non_success_via_panic() {
    let home = unique_temp("missing-src");
    write_config(
        &home,
        &format!(
            "[skills]\nsource = {:?}\ntargets = [{:?}]\nskip = []\n",
            home.join("no-such-skills"),
            home.join("out")
        ),
    );
    let out = run_in_home(&home, &["--check"]);
    assert!(
        !out.status.success(),
        "current behaviour: missing source does not exit 0 (it panics)"
    );
    // Do not pin 126/127; the panic path is a host-specific non-zero code.
    let combined = format!("{}{}", stdout(&out), stderr(&out));
    assert!(combined.contains("cannot read skills source"), "{combined}");
    fs::remove_dir_all(&home).ok();
}

#[test]
fn unknown_flag_behaves_as_default_sync() {
    let home = unique_temp("unknown");
    let source = home.join("receptors");
    fs::create_dir_all(&source).unwrap();
    write_skill(&source, "alpha", default_skill_body());
    let target = home.join("out");
    write_config(
        &home,
        &format!("[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = []\n"),
    );
    let out = run_in_home(&home, &["--not-a-real-flag"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(target.join("alpha").is_symlink());
    fs::remove_dir_all(&home).ok();
}

#[test]
fn malformed_mcp_source_does_not_fail_the_process() {
    let home = unique_temp("bad-mcp");
    let source = home.join("receptors");
    fs::create_dir_all(&source).unwrap();
    write_skill(&source, "alpha", default_skill_body());
    let skills_target = home.join("out");
    let mcp_src = home.join("mcp-servers.json");
    fs::write(&mcp_src, "{{{not json").unwrap();
    let oc = home.join("opencode/mcp.json");
    let cx = home.join("codex/config.toml");
    write_config(
        &home,
        &format!(
            "[skills]\nsource = {source:?}\ntargets = [{skills_target:?}]\nskip = []\n\n\
             [mcp]\nsource = {mcp_src:?}\nopencode = {oc:?}\ncodex = {cx:?}\n"
        ),
    );
    let out = run_in_home(&home, &["--full"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let combined = format!("{}{}", stdout(&out), stderr(&out));
    assert!(combined.contains("invalid JSON"), "{combined}");
    assert!(!oc.exists());
    fs::remove_dir_all(&home).ok();
}
