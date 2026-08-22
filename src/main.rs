use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, exit};

const DEFAULT_SKIP: &[&str] = &[
    "TEMPLATE.md",
    ".git",
    ".archive",
    ".cache",
    ".gitignore",
    "config.json",
];
const MCP_BEGIN: &str = "# synaxis-mcp-begin";
const MCP_END: &str = "# synaxis-mcp-end";
const DEFAULT_CONFIG: &str = r#"[skills]
source = "~/skills"
targets = [
    "~/.claude/skills",
    "~/.opencode/skills",
    "~/.codex/skills",
    "~/.agents/skills",
]
skip = ["TEMPLATE.md", ".git", ".archive", ".cache", ".gitignore", "config.json"]

[mcp]
source = "~/agent-config/mcp-servers.json"
opencode = "~/.opencode/mcp.json"
codex = "~/.codex/config.toml"

[ce]
source = "~/.claude/plugins/marketplaces/every-marketplace"
platforms = ["codex", "opencode", "gemini"]
"#;

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct Config {
    skills: Option<SkillsConfig>,
    mcp: Option<McpConfig>,
    ce: Option<CeConfig>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct SkillsConfig {
    source: Option<String>,
    targets: Option<Vec<SkillTarget>>,
    skip: Option<Vec<String>>,
}

/// A `[skills].targets` entry: either a plain path string (the historical
/// form) or an inline table carrying a path plus an optional allowlist of
/// skill directory names. A `Detailed` entry without `allow` links every
/// skill, exactly like `Plain`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SkillTarget {
    Plain(String),
    Detailed {
        path: String,
        allow: Option<Vec<String>>,
    },
}

/// A resolved sync target: its absolute path and, when it came from a
/// `Detailed` entry with an `allow` list, the set of permitted skill names.
#[derive(Debug, Clone, PartialEq)]
struct SyncTarget {
    path: PathBuf,
    allow: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct McpConfig {
    source: Option<String>,
    opencode: Option<String>,
    codex: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct CeConfig {
    source: Option<String>,
    platforms: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct McpServersFile {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: serde_json::Value,
    #[serde(default, rename = "_codexExtras")]
    codex_extras: BTreeMap<String, CodexExtra>,
}

#[derive(Debug, Deserialize)]
struct CodexExtra {
    url: String,
}

#[derive(Serialize)]
struct OpenCodeMcp {
    #[serde(rename = "mcpServers")]
    mcp_servers: serde_json::Value,
}

fn home() -> PathBuf {
    dirs::home_dir().expect("cannot resolve home directory")
}

fn config_path(home: &Path) -> PathBuf {
    home.join(".config/synaxis/config.toml")
}

fn expand_tilde(s: &str, home: &Path) -> PathBuf {
    if s == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(s)
}

fn load_config() -> Config {
    load_config_from(&home())
}

fn load_config_from(home: &Path) -> Config {
    let path = config_path(home);
    match fs::read_to_string(&path) {
        Ok(raw) => match toml::from_str::<Config>(&raw) {
            Ok(cfg) => cfg,
            Err(err) => {
                eprintln!(
                    "⚠  Config: invalid TOML at {}, using defaults ({})",
                    path.display(),
                    err
                );
                Config::default()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(err) => {
            eprintln!(
                "⚠  Config: cannot read {}, using defaults ({})",
                path.display(),
                err
            );
            Config::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Help,
    Init,
    Check,
    Sync { full: bool },
}

/// Flag parsing for the CLI. Help wins over init; init wins over the rest;
/// `--check` wins over `--full` (dry-run never mutates). Unknown flags are
/// ignored, matching the historical `args.iter().any(...)` checks in `main`.
fn parse_args(args: &[String]) -> Mode {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Mode::Help;
    }
    if args.iter().any(|a| a == "--init") {
        return Mode::Init;
    }
    let full = args.iter().any(|a| a == "--full");
    let check = args.iter().any(|a| a == "--check");
    if check {
        Mode::Check
    } else {
        Mode::Sync { full }
    }
}

fn skills_source(config: &Config, home: &Path) -> PathBuf {
    config
        .skills
        .as_ref()
        .and_then(|s| s.source.as_deref())
        .map(|s| expand_tilde(s, home))
        .unwrap_or_else(|| home.join("skills"))
}

fn targets(config: &Config, home: &Path) -> Vec<SyncTarget> {
    if let Some(entries) = config
        .skills
        .as_ref()
        .and_then(|s| s.targets.as_ref())
        .filter(|entries| !entries.is_empty())
    {
        return entries
            .iter()
            .map(|entry| match entry {
                SkillTarget::Plain(path) => SyncTarget {
                    path: expand_tilde(path, home),
                    allow: None,
                },
                SkillTarget::Detailed { path, allow } => SyncTarget {
                    path: expand_tilde(path, home),
                    allow: allow.clone(),
                },
            })
            .collect();
    }
    [
        ".claude/skills",
        ".opencode/skills",
        ".codex/skills",
        ".agents/skills",
    ]
    .iter()
    .map(|p| SyncTarget {
        path: home.join(p),
        allow: None,
    })
    .collect()
}

fn skip_entries(config: &Config) -> Vec<String> {
    if let Some(skip) = config.skills.as_ref().and_then(|s| s.skip.as_ref()) {
        return skip.clone();
    }
    DEFAULT_SKIP.iter().map(|s| (*s).to_string()).collect()
}

fn mcp_source(config: &Config, home: &Path) -> PathBuf {
    config
        .mcp
        .as_ref()
        .and_then(|m| m.source.as_deref())
        .map(|s| expand_tilde(s, home))
        .unwrap_or_else(|| home.join("agent-config/mcp-servers.json"))
}

fn mcp_opencode_path(config: &Config, home: &Path) -> PathBuf {
    config
        .mcp
        .as_ref()
        .and_then(|m| m.opencode.as_deref())
        .map(|s| expand_tilde(s, home))
        .unwrap_or_else(|| home.join(".opencode/mcp.json"))
}

fn mcp_codex_path(config: &Config, home: &Path) -> PathBuf {
    config
        .mcp
        .as_ref()
        .and_then(|m| m.codex.as_deref())
        .map(|s| expand_tilde(s, home))
        .unwrap_or_else(|| home.join(".codex/config.toml"))
}

fn ce_source(config: &Config, home: &Path) -> PathBuf {
    config
        .ce
        .as_ref()
        .and_then(|c| c.source.as_deref())
        .map(|s| expand_tilde(s, home))
        .unwrap_or_else(|| home.join(".claude/plugins/marketplaces/every-marketplace"))
}

fn ce_platforms(config: &Config) -> Vec<String> {
    if let Some(platforms) = config.ce.as_ref().and_then(|c| c.platforms.as_ref()) {
        return platforms.clone();
    }
    vec![
        "codex".to_string(),
        "opencode".to_string(),
        "gemini".to_string(),
    ]
}

fn sync_skills(home: &Path, config: &Config, dry_run: bool) -> usize {
    let skills_dir = skills_source(config, home);
    let targets = targets(config, home);
    let skip = skip_entries(config);

    let mut active_targets: Vec<SyncTarget> = Vec::new();
    if !dry_run {
        for t in &targets {
            if t.path == skills_dir {
                eprintln!(
                    "⚠  Skills: target {} is the source directory; skipping",
                    t.path.display()
                );
                continue;
            }
            if t.path.is_symlink() {
                if let Err(err) = fs::remove_file(&t.path) {
                    eprintln!(
                        "⚠  Skills: cannot replace target symlink {} ({})",
                        t.path.display(),
                        err
                    );
                    continue;
                }
            }
            if let Err(err) = fs::create_dir_all(&t.path) {
                eprintln!(
                    "⚠  Skills: cannot create target {} ({})",
                    t.path.display(),
                    err
                );
                continue;
            }
            active_targets.push(t.clone());
        }
        // Resolve the source against symlinks so "is this entry one of ours?"
        // compares real paths on both sides.
        let source_dir = fs::canonicalize(&skills_dir).unwrap_or_else(|_| skills_dir.clone());
        for t in &active_targets {
            if let Ok(entries) = fs::read_dir(&t.path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_symlink() && !path.exists() {
                        fs::remove_file(&path).ok();
                        continue;
                    }
                    // Prune stale managed links in allowlisted targets. Only
                    // ever remove a symlink whose resolved destination lives
                    // inside the skills source and whose name is no longer
                    // allowed; real files, real directories, and foreign
                    // symlinks are left untouched.
                    let Some(allow) = t.allow.as_ref() else {
                        continue;
                    };
                    if !path.is_symlink() {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().to_string();
                    if allow.iter().any(|a| a == &name) {
                        continue;
                    }
                    match fs::canonicalize(&path) {
                        Ok(resolved) if resolved.starts_with(&source_dir) => {
                            fs::remove_file(&path).ok();
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let mut count = 0;
    let mut entries: Vec<_> = fs::read_dir(&skills_dir)
        .unwrap_or_else(|err| {
            panic!(
                "cannot read skills source {} ({})",
                skills_dir.display(),
                err
            )
        })
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let item = entry.path();
        let name = item.file_name().unwrap().to_string_lossy();
        if skip.iter().any(|s| s == name.as_ref()) {
            continue;
        }
        if item.is_dir() && item.join("SKILL.md").exists() {
            let has_frontmatter = fs::read_to_string(item.join("SKILL.md"))
                .map(|c| c.starts_with("---"))
                .unwrap_or(false);
            if !has_frontmatter {
                eprintln!("⚠  Skills: {}/SKILL.md missing YAML frontmatter", name);
            }
            if dry_run {
                let flag = if has_frontmatter {
                    ""
                } else {
                    "  ⚠ no frontmatter"
                };
                println!("  {}{}", name, flag);
            } else {
                for t in &active_targets {
                    // An allowlisted target only receives the named skills.
                    if let Some(allow) = t.allow.as_ref()
                        && !allow.iter().any(|a| name.as_ref() == a.as_str())
                    {
                        continue;
                    }
                    let dest = t.path.join(name.as_ref());
                    if dest.is_symlink() || dest.exists() {
                        if dest.is_dir() && !dest.is_symlink() {
                            fs::remove_dir_all(&dest).ok();
                        } else {
                            fs::remove_file(&dest).ok();
                        }
                    }
                    if let Err(err) = std::os::unix::fs::symlink(&item, &dest) {
                        eprintln!(
                            "⚠  Skills: cannot link {} -> {} ({})",
                            dest.display(),
                            item.display(),
                            err
                        );
                    }
                }
            }
            count += 1;
        }
    }
    count
}

fn sync_mcp(home: &Path, config: &Config) {
    let source = mcp_source(config, home);
    let raw = match fs::read_to_string(&source) {
        Ok(content) => content,
        Err(_) => {
            eprintln!("⚠  MCP: cannot read {}, skipping", source.display());
            return;
        }
    };

    let parsed: McpServersFile = match serde_json::from_str(&raw) {
        Ok(data) => data,
        Err(_) => {
            eprintln!("⚠  MCP: invalid JSON in {}, skipping", source.display());
            return;
        }
    };

    let opencode_path = mcp_opencode_path(config, home);
    if let Some(parent) = opencode_path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let opencode_body = OpenCodeMcp {
        mcp_servers: normalize_mcp_servers(parsed.mcp_servers.clone()),
    };
    match serde_json::to_string_pretty(&opencode_body) {
        Ok(json) => {
            if fs::write(&opencode_path, format!("{json}\n")).is_ok() {
                println!("✓  MCP: wrote OpenCode config");
            } else {
                eprintln!("⚠  MCP: failed writing OpenCode config");
            }
        }
        Err(_) => eprintln!("⚠  MCP: failed encoding OpenCode config"),
    }

    let codex_path = mcp_codex_path(config, home);
    if let Some(parent) = codex_path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let existing = fs::read_to_string(&codex_path).unwrap_or_default();
    let managed_block = build_codex_mcp_block(&parsed.codex_extras);
    let updated = match replace_or_append_managed_block(&existing, &managed_block) {
        Ok(content) => content,
        Err(msg) => {
            eprintln!("⚠  MCP: {}", msg);
            return;
        }
    };
    if fs::write(&codex_path, updated).is_ok() {
        println!("✓  MCP: wrote Codex config block");
    } else {
        eprintln!("⚠  MCP: failed writing Codex config");
    }
}

/// Render a TOML key as-is if it is a valid bare key (`[A-Za-z0-9_-]`),
/// otherwise as a quoted basic string. Without this a server name containing a
/// dot would be parsed as a nested table rather than a single server name.
fn toml_key(key: &str) -> String {
    let is_bare = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if is_bare {
        key.to_string()
    } else {
        serde_json::to_string(key).unwrap_or_else(|_| format!("\"{}\"", key))
    }
}

/// OpenCode expects `mcpServers` to be a JSON object. A source file that omits
/// the key (tolerated by `#[serde(default)]`) or sets it to `null` deserializes
/// to `Value::Null`; a malformed source may also set it to an array or scalar.
/// Writing any of these through emits e.g. `"mcpServers": null` or
/// `"mcpServers": []`, which the consumer rejects. Normalize any non-object
/// value to an empty object so a partial or malformed source still yields a
/// valid OpenCode config.
fn normalize_mcp_servers(value: serde_json::Value) -> serde_json::Value {
    if value.is_object() {
        value
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    }
}

fn build_codex_mcp_block(codex_extras: &BTreeMap<String, CodexExtra>) -> String {
    let mut out = String::new();
    out.push_str(MCP_BEGIN);
    out.push('\n');
    for (name, extra) in codex_extras {
        out.push_str(&format!("[mcp_servers.{}]\n", toml_key(name)));
        let url = serde_json::to_string(&extra.url).unwrap_or_else(|_| "\"\"".to_string());
        out.push_str(&format!("url = {}\n", url));
    }
    out.push_str(MCP_END);
    out.push('\n');
    out
}

fn replace_or_append_managed_block(existing: &str, block: &str) -> Result<String, &'static str> {
    let mut out = String::new();
    let mut rest = existing;
    let mut inserted = false;

    while let Some(start) = rest.find(MCP_BEGIN) {
        out.push_str(&rest[..start]);
        let block_start = &rest[start..];
        if let Some(end_rel) = block_start.find(MCP_END) {
            let mut end = start + end_rel + MCP_END.len();
            // The managed `block` carries its own trailing newline, so consume a
            // single newline following MCP_END here; otherwise it survives as
            // leftover and a blank line accumulates on every run (non-idempotent).
            if rest[end..].starts_with('\n') {
                end += 1;
            }
            if !inserted {
                out.push_str(block);
                inserted = true;
            }
            rest = &rest[end..];
        } else {
            return Err(
                "found '# synaxis-mcp-begin' without matching '# synaxis-mcp-end'; leaving Codex config unchanged",
            );
        }
    }

    if inserted {
        out.push_str(rest);
        return Ok(out);
    }

    let mut out = existing.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(block);
    Ok(out)
}

fn sync_ce(home: &Path, config: &Config) {
    let ce_dir = ce_source(config, home);
    if !ce_dir.exists() {
        eprintln!("⚠  CE: every-marketplace not installed, skipping");
        return;
    }

    for target in ce_platforms(config) {
        let status = Command::new("bunx")
            .args([
                "@every-env/compound-plugin",
                "install",
                "compound-engineering",
                "--to",
                target.as_str(),
            ])
            .current_dir(&ce_dir)
            .status();
        match status {
            Ok(s) if s.success() => {
                println!("✓  CE: compound-engineering installed to {}", target)
            }
            _ => eprintln!(
                "⚠  CE: compound-engineering install failed for {}, continuing",
                target
            ),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let home = home();
    let cfg_path = config_path(&home);

    match parse_args(&args) {
        Mode::Help => {
            println!("Usage: synaxis [--full] [--check] [--init]");
            println!();
            println!("  (default)  Sync skills symlinks (fast, safe for git hooks)");
            println!("  --full     Skills + MCP + compound-engineering");
            println!("  --check    Dry run — list skills, no changes");
            println!("  --init     Create default config at ~/.config/synaxis/config.toml");
            println!();
            println!("Config file: {}", cfg_path.display());
            exit(0);
        }
        Mode::Init => {
            if cfg_path.exists() {
                println!("Config already exists at {}", cfg_path.display());
                exit(0);
            }
            if let Some(parent) = cfg_path.parent() {
                if let Err(err) = fs::create_dir_all(parent) {
                    eprintln!("failed to create {} ({})", parent.display(), err);
                    exit(1);
                }
            }
            if let Err(err) = fs::write(&cfg_path, DEFAULT_CONFIG) {
                eprintln!("failed to write {} ({})", cfg_path.display(), err);
                exit(1);
            }
            println!("Wrote default config to {}", cfg_path.display());
            exit(0);
        }
        Mode::Check => {
            let config = load_config();
            println!("Skills in {}:", skills_source(&config, &home).display());
            let count = sync_skills(&home, &config, true);
            println!("{} skills", count);
            exit(0);
        }
        Mode::Sync { full } => {
            let config = load_config();
            let count = sync_skills(&home, &config, false);
            eprintln!("✓  Skills: {} synced", count);

            if full {
                sync_mcp(&home, &config);
                sync_ce(&home, &config);
                eprintln!("Done. Restart Codex/OpenCode to pick up changes.");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[test]
    fn test_expand_tilde_home_only() {
        let home = PathBuf::from("/home/user");
        assert_eq!(expand_tilde("~", &home), PathBuf::from("/home/user"));
    }

    #[test]
    fn test_expand_tilde_home_relative() {
        let home = PathBuf::from("/home/user");
        assert_eq!(
            expand_tilde("~/foo/bar", &home),
            PathBuf::from("/home/user/foo/bar")
        );
    }

    #[test]
    fn test_expand_tilde_absolute_passthrough() {
        let home = PathBuf::from("/home/user");
        assert_eq!(
            expand_tilde("/absolute/path", &home),
            PathBuf::from("/absolute/path")
        );
    }

    #[test]
    fn test_default_targets_use_home() {
        let home = PathBuf::from("/home/user");
        let config = Config::default();
        let resolved = targets(&config, &home);
        assert_eq!(
            resolved.iter().map(|t| t.path.clone()).collect::<Vec<_>>(),
            vec![
                PathBuf::from("/home/user/.claude/skills"),
                PathBuf::from("/home/user/.opencode/skills"),
                PathBuf::from("/home/user/.codex/skills"),
                PathBuf::from("/home/user/.agents/skills"),
            ]
        );
        // Default targets are plain: no allow set anywhere.
        assert!(resolved.iter().all(|t| t.allow.is_none()));
    }

    /// Create a throwaway directory under the system temp dir for a test.
    fn test_temp_dir(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "synaxis-test-{tag}-{}-{unique}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Create a minimal valid skill directory under `source`.
    fn test_write_skill(source: &Path, name: &str) {
        let dir = source.join(name);
        fs::create_dir_all(&dir).expect("create skill dir");
        fs::write(dir.join("SKILL.md"), "---\nname: test\n---\nbody\n").expect("write SKILL.md");
    }

    fn test_config_from_toml(raw: &str) -> Config {
        toml::from_str::<Config>(raw).expect("valid test config")
    }

    #[test]
    fn test_plain_string_targets_sync_every_skill() {
        // Backward compatibility: a plain string target links every skill.
        let tmp = test_temp_dir("plain-target");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "alpha");
        test_write_skill(&source, "beta");
        let target = tmp.join("target");
        let raw = format!("[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = []\n");
        let config = test_config_from_toml(&raw);

        let count = sync_skills(&tmp, &config, false);

        assert_eq!(count, 2, "both source skills are processed");
        for name in ["alpha", "beta"] {
            let dest = target.join(name);
            assert!(dest.is_symlink(), "{name} should be a symlink");
            assert_eq!(fs::read_link(&dest).unwrap(), source.join(name));
        }
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_detailed_target_with_allow_links_only_allowed_skills() {
        // A target with an allow list receives symlinks only for named skills,
        // but the printed count still covers every source skill.
        let tmp = test_temp_dir("allow-target");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "alpha");
        test_write_skill(&source, "beta");
        let target = tmp.join("target");
        let raw = format!(
            "[skills]\nsource = {source:?}\ntargets = [{{ path = {target:?}, allow = [\"alpha\"] }}]\nskip = []\n"
        );
        let config = test_config_from_toml(&raw);

        let count = sync_skills(&tmp, &config, false);

        assert_eq!(count, 2, "count tracks source skills, not links");
        let alpha = target.join("alpha");
        assert!(alpha.is_symlink(), "allowed skill should be linked");
        assert_eq!(fs::read_link(&alpha).unwrap(), source.join("alpha"));
        assert!(
            target.join("beta").symlink_metadata().is_err(),
            "disallowed skill must not be linked"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_detailed_target_without_allow_behaves_like_plain() {
        // `{ path = ... }` with no allow key links everything, and the pruning
        // pass never removes pre-existing source-derived links.
        let tmp = test_temp_dir("no-allow");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "alpha");
        let target = tmp.join("target");
        fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(source.join("alpha"), target.join("alpha"))
            .expect("pre-link alpha");
        let raw = format!(
            "[skills]\nsource = {source:?}\ntargets = [{{ path = {target:?} }}]\nskip = []\n"
        );
        let config = test_config_from_toml(&raw);

        sync_skills(&tmp, &config, false);

        assert!(target.join("alpha").is_symlink());
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_allowlisted_target_prunes_stale_source_derived_symlink() {
        // A pre-existing symlink into the skills source whose name is not in
        // the allow list is pruned on the next sync.
        let tmp = test_temp_dir("prune-stale");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "alpha");
        test_write_skill(&source, "beta");
        let target = tmp.join("target");
        fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(source.join("beta"), target.join("beta"))
            .expect("pre-link stale beta");
        let raw = format!(
            "[skills]\nsource = {source:?}\ntargets = [{{ path = {target:?}, allow = [\"alpha\"] }}]\nskip = []\n"
        );
        let config = test_config_from_toml(&raw);

        sync_skills(&tmp, &config, false);

        assert!(
            target.join("beta").symlink_metadata().is_err(),
            "source-derived symlink not in allow must be pruned"
        );
        assert!(
            target.join("alpha").is_symlink(),
            "allowed skill still linked"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_allowlisted_target_leaves_foreign_symlink_and_real_dir_untouched() {
        // A symlink pointing outside the skills source and a real directory in
        // the target must survive a sync with an allow list.
        let tmp = test_temp_dir("prune-conservative");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "alpha");
        let outside = tmp.join("elsewhere");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep.txt"), "data").unwrap();
        let target = tmp.join("target");
        fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&outside, target.join("external")).expect("pre-link foreign");
        let real = target.join("realskill");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("notes.md"), "hand made").unwrap();
        let raw = format!(
            "[skills]\nsource = {source:?}\ntargets = [{{ path = {target:?}, allow = [\"alpha\"] }}]\nskip = []\n"
        );
        let config = test_config_from_toml(&raw);

        sync_skills(&tmp, &config, false);

        let external = target.join("external");
        assert!(external.is_symlink(), "foreign symlink must stay a symlink");
        assert_eq!(fs::read_link(&external).unwrap(), outside);
        assert!(
            real.is_dir() && !real.is_symlink(),
            "real directory must stay a real directory"
        );
        assert!(
            real.join("notes.md").is_file(),
            "real directory contents survive"
        );
        assert!(target.join("alpha").is_symlink());
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_default_skip_entries() {
        let config = Config::default();
        let expected: Vec<String> = DEFAULT_SKIP.iter().map(|s| s.to_string()).collect();
        assert_eq!(skip_entries(&config), expected);
    }

    #[test]
    fn test_default_ce_platforms() {
        let config = Config::default();
        assert_eq!(
            ce_platforms(&config),
            vec![
                "codex".to_string(),
                "opencode".to_string(),
                "gemini".to_string()
            ]
        );
    }

    #[test]
    fn test_replace_or_append_managed_block_appends_to_empty() {
        let block = format!("{}\nsome content\n{}\n", MCP_BEGIN, MCP_END);
        let result = replace_or_append_managed_block("", &block).unwrap();
        assert_eq!(result, block);
    }

    #[test]
    fn test_replace_or_append_managed_block_appends_after_existing_without_newline() {
        let existing = "existing content";
        let block = format!("{}\nsome content\n{}\n", MCP_BEGIN, MCP_END);
        let result = replace_or_append_managed_block(existing, &block).unwrap();
        assert_eq!(result, format!("existing content\n{}", block));
    }

    #[test]
    fn test_replace_or_append_managed_block_replaces_existing() {
        let existing = format!("{}\nold content\n{}", MCP_BEGIN, MCP_END);
        let block = format!("{}\nnew content\n{}\n", MCP_BEGIN, MCP_END);
        let result = replace_or_append_managed_block(&existing, &block).unwrap();
        assert_eq!(result, block);
    }

    #[test]
    fn test_replace_or_append_managed_block_collapses_duplicate_blocks() {
        // A config that accumulated two managed blocks (e.g. from a crashed run)
        // must collapse to a single fresh block, preserving surrounding content.
        let existing = format!(
            "{b}\nold1\n{e}\nmiddle\n{b}\nold2\n{e}\n",
            b = MCP_BEGIN,
            e = MCP_END
        );
        let block = format!("{}\nnew\n{}\n", MCP_BEGIN, MCP_END);
        let result = replace_or_append_managed_block(&existing, &block).unwrap();
        assert_eq!(
            result.matches(MCP_BEGIN).count(),
            1,
            "exactly one begin marker"
        );
        assert_eq!(result.matches(MCP_END).count(), 1, "exactly one end marker");
        assert!(result.contains("new"));
        assert!(result.contains("middle"));
        assert!(!result.contains("old1"));
        assert!(!result.contains("old2"));
    }

    #[test]
    fn test_replace_or_append_managed_block_is_idempotent() {
        // Steady state: config.toml already ends with the managed block. Running
        // synaxis again must reproduce the file byte-for-byte, not accumulate a
        // trailing blank line on every run.
        let block = format!("{}\nnew\n{}\n", MCP_BEGIN, MCP_END);
        let once = replace_or_append_managed_block("model = \"gpt-5\"\n", &block).unwrap();
        let twice = replace_or_append_managed_block(&once, &block).unwrap();
        assert_eq!(once, twice, "re-applying the managed block must be a no-op");
    }

    #[test]
    fn test_replace_or_append_managed_block_errors_on_unclosed_begin() {
        let existing = format!("{}\nno end marker here", MCP_BEGIN);
        let block = format!("{}\nnew\n{}\n", MCP_BEGIN, MCP_END);
        assert!(replace_or_append_managed_block(&existing, &block).is_err());
    }

    #[test]
    fn test_normalize_mcp_servers_null_becomes_empty_object() {
        // A source missing `mcpServers` deserializes to null; writing it through
        // would emit `"mcpServers": null`, which OpenCode rejects.
        let normalized = normalize_mcp_servers(serde_json::Value::Null);
        assert_eq!(normalized, serde_json::json!({}));
        // And it serializes inside the OpenCode body as an object, not null.
        let body = OpenCodeMcp {
            mcp_servers: normalized,
        };
        let json = serde_json::to_string(&body).unwrap();
        assert_eq!(json, r#"{"mcpServers":{}}"#);
    }

    #[test]
    fn test_normalize_mcp_servers_preserves_existing_object() {
        let value = serde_json::json!({ "ctx7": { "url": "https://example.com" } });
        assert_eq!(normalize_mcp_servers(value.clone()), value);
    }

    #[test]
    fn test_normalize_mcp_servers_non_object_becomes_empty_object() {
        // OpenCode's `mcpServers` must be a JSON object. A source that sets it to
        // any other shape — an empty array (a common "no servers" mistake), or a
        // scalar from a malformed file — would otherwise be written through
        // verbatim as e.g. `"mcpServers": []`, which OpenCode rejects. Coerce any
        // non-object value to an empty object, just as the null case does.
        for malformed in [
            serde_json::json!([]),
            serde_json::json!([{ "url": "https://example.com" }]),
            serde_json::json!("oops"),
            serde_json::json!(7),
            serde_json::json!(true),
        ] {
            assert_eq!(
                normalize_mcp_servers(malformed.clone()),
                serde_json::json!({}),
                "non-object {malformed} should normalize to empty object"
            );
        }
    }

    #[test]
    fn test_build_codex_mcp_block_escapes_url() {
        let mut extras = BTreeMap::new();
        extras.insert(
            "my-server".to_string(),
            CodexExtra {
                url: "https://example.com/path?q=1&r=2".to_string(),
            },
        );
        let block = build_codex_mcp_block(&extras);
        assert!(block.starts_with(MCP_BEGIN));
        assert!(block.trim_end().ends_with(MCP_END));
        assert!(block.contains("[mcp_servers.my-server]"));
        assert!(block.contains(r#"url = "https://example.com/path?q=1&r=2""#));
    }

    #[test]
    fn test_build_codex_mcp_block_quotes_non_bare_key() {
        // A server name containing a dot is not a valid TOML bare key; left
        // unquoted it would be parsed as a nested table (mcp_servers.my.server)
        // rather than a single server named "my.server".
        let mut extras = BTreeMap::new();
        extras.insert(
            "my.server".to_string(),
            CodexExtra {
                url: "https://example.com".to_string(),
            },
        );
        let block = build_codex_mcp_block(&extras);
        assert!(
            block.contains(r#"[mcp_servers."my.server"]"#),
            "non-bare key should be quoted, got: {block}"
        );
    }

    fn args(flags: &[&str]) -> Vec<String> {
        flags.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn test_parse_args_empty_is_skills_only_sync() {
        assert_eq!(parse_args(&args(&[])), Mode::Sync { full: false });
    }

    #[test]
    fn test_parse_args_full() {
        assert_eq!(parse_args(&args(&["--full"])), Mode::Sync { full: true });
    }

    #[test]
    fn test_parse_args_check() {
        assert_eq!(parse_args(&args(&["--check"])), Mode::Check);
    }

    #[test]
    fn test_parse_args_init() {
        assert_eq!(parse_args(&args(&["--init"])), Mode::Init);
    }

    #[test]
    fn test_parse_args_help_long_and_short() {
        assert_eq!(parse_args(&args(&["--help"])), Mode::Help);
        assert_eq!(parse_args(&args(&["-h"])), Mode::Help);
    }

    #[test]
    fn test_parse_args_help_wins_over_other_flags() {
        assert_eq!(
            parse_args(&args(&["--init", "--help", "--full"])),
            Mode::Help
        );
        assert_eq!(parse_args(&args(&["-h", "--check"])), Mode::Help);
    }

    #[test]
    fn test_parse_args_init_wins_over_sync_flags() {
        assert_eq!(parse_args(&args(&["--full", "--init"])), Mode::Init);
    }

    #[test]
    fn test_parse_args_check_wins_over_full() {
        assert_eq!(parse_args(&args(&["--full", "--check"])), Mode::Check);
        assert_eq!(parse_args(&args(&["--check", "--full"])), Mode::Check);
    }

    #[test]
    fn test_parse_args_unknown_flags_are_ignored() {
        assert_eq!(
            parse_args(&args(&["--unknown", "--also-not-a-flag"])),
            Mode::Sync { full: false }
        );
        assert_eq!(
            parse_args(&args(&["--full", "--unknown"])),
            Mode::Sync { full: true }
        );
    }

    #[test]
    fn test_expand_tilde_empty_and_relative_and_unicode() {
        let home = PathBuf::from("/home/user");
        assert_eq!(expand_tilde("", &home), PathBuf::from(""));
        assert_eq!(
            expand_tilde("relative/path", &home),
            PathBuf::from("relative/path")
        );
        assert_eq!(expand_tilde("~/", &home), home.clone());
        assert_eq!(
            expand_tilde("~/技能/foo", &home),
            PathBuf::from("/home/user/技能/foo")
        );
        // `~name` is not a home expansion; only `~` and `~/...` are.
        assert_eq!(expand_tilde("~name/foo", &home), PathBuf::from("~name/foo"));
    }

    #[test]
    fn test_config_path_is_under_home_config() {
        let home = PathBuf::from("/home/user");
        assert_eq!(
            config_path(&home),
            PathBuf::from("/home/user/.config/synaxis/config.toml")
        );
    }

    #[test]
    fn test_skills_source_default_and_override() {
        let home = PathBuf::from("/home/user");
        assert_eq!(
            skills_source(&Config::default(), &home),
            PathBuf::from("/home/user/skills")
        );
        let config = test_config_from_toml("[skills]\nsource = \"~/receptors\"\n");
        assert_eq!(
            skills_source(&config, &home),
            PathBuf::from("/home/user/receptors")
        );
    }

    #[test]
    fn test_mcp_and_ce_path_defaults_and_overrides() {
        let home = PathBuf::from("/home/user");
        let default = Config::default();
        assert_eq!(
            mcp_source(&default, &home),
            PathBuf::from("/home/user/agent-config/mcp-servers.json")
        );
        assert_eq!(
            mcp_opencode_path(&default, &home),
            PathBuf::from("/home/user/.opencode/mcp.json")
        );
        assert_eq!(
            mcp_codex_path(&default, &home),
            PathBuf::from("/home/user/.codex/config.toml")
        );
        assert_eq!(
            ce_source(&default, &home),
            PathBuf::from("/home/user/.claude/plugins/marketplaces/every-marketplace")
        );

        let config = test_config_from_toml(
            "[mcp]\nsource = \"~/m.json\"\nopencode = \"~/o.json\"\ncodex = \"~/c.toml\"\n\n[ce]\nsource = \"~/ce\"\nplatforms = [\"codex\"]\n",
        );
        assert_eq!(
            mcp_source(&config, &home),
            PathBuf::from("/home/user/m.json")
        );
        assert_eq!(
            mcp_opencode_path(&config, &home),
            PathBuf::from("/home/user/o.json")
        );
        assert_eq!(
            mcp_codex_path(&config, &home),
            PathBuf::from("/home/user/c.toml")
        );
        assert_eq!(ce_source(&config, &home), PathBuf::from("/home/user/ce"));
        assert_eq!(ce_platforms(&config), vec!["codex".to_string()]);
    }

    #[test]
    fn test_skip_entries_override_and_explicit_empty() {
        let custom = test_config_from_toml("[skills]\nskip = [\"foo\", \"bar\"]\n");
        assert_eq!(
            skip_entries(&custom),
            vec!["foo".to_string(), "bar".to_string()]
        );
        let empty = test_config_from_toml("[skills]\nskip = []\n");
        assert_eq!(skip_entries(&empty), Vec::<String>::new());
    }

    #[test]
    fn test_empty_ce_platforms_does_not_fall_back() {
        let config = test_config_from_toml("[ce]\nplatforms = []\n");
        assert_eq!(ce_platforms(&config), Vec::<String>::new());
    }

    #[test]
    fn test_mixed_plain_and_detailed_targets() {
        let home = PathBuf::from("/home/user");
        let config = test_config_from_toml(
            "[skills]\ntargets = [\"~/.claude/skills\", { path = \"~/.pi/agent/skills\", allow = [\"glycolysis\"] }]\n",
        );
        let resolved = targets(&config, &home);
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].path, PathBuf::from("/home/user/.claude/skills"));
        assert_eq!(resolved[0].allow, None);
        assert_eq!(
            resolved[1].path,
            PathBuf::from("/home/user/.pi/agent/skills")
        );
        assert_eq!(
            resolved[1].allow.as_deref(),
            Some(&["glycolysis".to_string()][..])
        );
    }

    #[test]
    fn test_toml_key_bare_and_quoted() {
        assert_eq!(toml_key("my-server_1"), "my-server_1");
        assert_eq!(toml_key("my.server"), r#""my.server""#);
        assert_eq!(toml_key("has space"), r#""has space""#);
        assert_eq!(toml_key(""), r#""""#);
        assert_eq!(toml_key("quote\"me"), r#""quote\"me""#);
    }

    #[test]
    fn test_build_codex_mcp_block_empty_extras() {
        let extras = BTreeMap::new();
        let block = build_codex_mcp_block(&extras);
        assert_eq!(block, format!("{MCP_BEGIN}\n{MCP_END}\n"));
    }

    #[test]
    fn test_replace_or_append_managed_block_preserves_surrounding() {
        let existing = format!("prefix\n{MCP_BEGIN}\nold\n{MCP_END}\nsuffix\n");
        let block = format!("{MCP_BEGIN}\nnew\n{MCP_END}\n");
        let result = replace_or_append_managed_block(&existing, &block).unwrap();
        assert_eq!(result, format!("prefix\n{block}suffix\n"));
    }

    fn write_config_file(home: &Path, body: &str) {
        let path = config_path(home);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn test_load_config_from_missing_file_is_default() {
        let tmp = test_temp_dir("cfg-missing");
        let cfg = load_config_from(&tmp);
        assert!(cfg.skills.is_none());
        assert!(cfg.mcp.is_none());
        assert!(cfg.ce.is_none());
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_load_config_from_valid_file() {
        let tmp = test_temp_dir("cfg-valid");
        write_config_file(&tmp, "[skills]\nsource = \"~/receptors\"\n");
        let cfg = load_config_from(&tmp);
        assert_eq!(
            cfg.skills.as_ref().and_then(|s| s.source.as_deref()),
            Some("~/receptors")
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_load_config_from_unreadable_uses_defaults() {
        let tmp = test_temp_dir("cfg-unreadable");
        write_config_file(&tmp, "[skills]\nsource = \"~/receptors\"\n");
        let path = config_path(&tmp);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        }
        let cfg = load_config_from(&tmp);
        assert!(
            cfg.skills.is_none(),
            "unreadable config must fail-open to defaults"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).ok();
        }
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_skip_entries_prevent_sync() {
        let tmp = test_temp_dir("skip-sync");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "keep");
        test_write_skill(&source, "TEMPLATE.md");
        let target = tmp.join("target");
        let raw = format!(
            "[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = [\"TEMPLATE.md\"]\n"
        );
        let config = test_config_from_toml(&raw);

        let count = sync_skills(&tmp, &config, false);

        assert_eq!(count, 1);
        assert!(target.join("keep").is_symlink());
        assert!(target.join("TEMPLATE.md").symlink_metadata().is_err());
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_non_skill_entries_are_ignored() {
        let tmp = test_temp_dir("nonskill");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "real");
        fs::write(source.join("README.md"), "nope").unwrap();
        fs::create_dir_all(source.join("empty")).unwrap();
        let target = tmp.join("target");
        let raw = format!("[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = []\n");
        let config = test_config_from_toml(&raw);

        let count = sync_skills(&tmp, &config, false);

        assert_eq!(count, 1);
        assert!(target.join("real").is_symlink());
        assert!(!target.join("README.md").exists());
        assert!(!target.join("empty").exists());
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_unicode_skill_name_is_linked() {
        let tmp = test_temp_dir("unicode");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "技能");
        let target = tmp.join("target");
        let raw = format!("[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = []\n");
        let config = test_config_from_toml(&raw);

        let count = sync_skills(&tmp, &config, false);

        assert_eq!(count, 1);
        let dest = target.join("技能");
        assert!(dest.is_symlink());
        assert_eq!(fs::read_link(&dest).unwrap(), source.join("技能"));
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_missing_frontmatter_still_syncs() {
        let tmp = test_temp_dir("no-fm");
        let source = tmp.join("skills");
        let skill = source.join("bare");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "no frontmatter here\n").unwrap();
        let target = tmp.join("target");
        let raw = format!("[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = []\n");
        let config = test_config_from_toml(&raw);

        let count = sync_skills(&tmp, &config, false);

        assert_eq!(count, 1);
        assert!(target.join("bare").is_symlink());
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_dry_run_does_not_create_targets_or_links() {
        let tmp = test_temp_dir("dry");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "alpha");
        let target = tmp.join("target");
        let raw = format!("[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = []\n");
        let config = test_config_from_toml(&raw);

        let count = sync_skills(&tmp, &config, true);

        assert_eq!(count, 1);
        assert!(
            !target.exists(),
            "dry run must not create the target directory"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_source_equal_target_is_skipped() {
        let tmp = test_temp_dir("src-eq-tgt");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "alpha");
        let raw = format!("[skills]\nsource = {source:?}\ntargets = [{source:?}]\nskip = []\n");
        let config = test_config_from_toml(&raw);

        let count = sync_skills(&tmp, &config, false);

        assert_eq!(count, 1);
        // Source dir must remain a real directory of skills, not a place we
        // planted a self-symlink named alpha-over-alpha.
        assert!(source.join("alpha").is_dir());
        assert!(!source.join("alpha").is_symlink());
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_target_dir_level_symlink_is_replaced_with_real_dir() {
        let tmp = test_temp_dir("tgt-symlink");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "alpha");
        let elsewhere = tmp.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let target = tmp.join("target");
        std::os::unix::fs::symlink(&elsewhere, &target).unwrap();
        let raw = format!("[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = []\n");
        let config = test_config_from_toml(&raw);

        sync_skills(&tmp, &config, false);

        assert!(target.is_dir() && !target.is_symlink());
        assert!(target.join("alpha").is_symlink());
        assert!(
            elsewhere.exists(),
            "the old symlink destination is not deleted"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_empty_allow_links_nothing_and_prunes_stale() {
        let tmp = test_temp_dir("empty-allow");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "alpha");
        let target = tmp.join("target");
        fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(source.join("alpha"), target.join("alpha")).unwrap();
        let raw = format!(
            "[skills]\nsource = {source:?}\ntargets = [{{ path = {target:?}, allow = [] }}]\nskip = []\n"
        );
        let config = test_config_from_toml(&raw);

        let count = sync_skills(&tmp, &config, false);

        assert_eq!(count, 1);
        assert!(
            target.join("alpha").symlink_metadata().is_err(),
            "empty allow must prune source-derived links"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    fn mcp_config(source: &Path, opencode: &Path, codex: &Path) -> Config {
        let raw =
            format!("[mcp]\nsource = {source:?}\nopencode = {opencode:?}\ncodex = {codex:?}\n");
        test_config_from_toml(&raw)
    }

    #[test]
    fn test_sync_mcp_writes_opencode_and_codex() {
        let tmp = test_temp_dir("mcp-happy");
        let source = tmp.join("mcp-servers.json");
        let opencode = tmp.join("opencode/mcp.json");
        let codex = tmp.join("codex/config.toml");
        fs::write(
            &source,
            r#"{
                "mcpServers": { "fs": { "command": "npx" } },
                "_codexExtras": { "context7": { "url": "https://mcp.context7.com/mcp" } }
            }"#,
        )
        .unwrap();
        let config = mcp_config(&source, &opencode, &codex);

        sync_mcp(&tmp, &config);

        let oc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&opencode).unwrap()).unwrap();
        assert_eq!(oc["mcpServers"]["fs"]["command"], "npx");
        let cx = fs::read_to_string(&codex).unwrap();
        assert!(cx.contains(MCP_BEGIN));
        assert!(cx.contains("[mcp_servers.context7]"));
        assert!(cx.contains("https://mcp.context7.com/mcp"));
        assert!(cx.contains(MCP_END));
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_sync_mcp_missing_source_skips_writes() {
        let tmp = test_temp_dir("mcp-missing");
        let source = tmp.join("no-such.json");
        let opencode = tmp.join("opencode/mcp.json");
        let codex = tmp.join("codex/config.toml");
        let config = mcp_config(&source, &opencode, &codex);

        sync_mcp(&tmp, &config);

        assert!(!opencode.exists());
        assert!(!codex.exists());
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_sync_mcp_invalid_json_skips_writes() {
        let tmp = test_temp_dir("mcp-badjson");
        let source = tmp.join("mcp-servers.json");
        fs::write(&source, "not json {").unwrap();
        let opencode = tmp.join("opencode/mcp.json");
        let codex = tmp.join("codex/config.toml");
        let config = mcp_config(&source, &opencode, &codex);

        sync_mcp(&tmp, &config);

        assert!(!opencode.exists());
        assert!(!codex.exists());
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_sync_mcp_unclosed_block_leaves_codex_file_bytes_unchanged() {
        let tmp = test_temp_dir("mcp-unclosed");
        let source = tmp.join("mcp-servers.json");
        fs::write(
            &source,
            r#"{ "_codexExtras": { "x": { "url": "https://example.com" } } }"#,
        )
        .unwrap();
        let opencode = tmp.join("opencode/mcp.json");
        let codex = tmp.join("codex/config.toml");
        fs::create_dir_all(codex.parent().unwrap()).unwrap();
        let original = format!("{MCP_BEGIN}\nno end marker\n");
        fs::write(&codex, &original).unwrap();
        let config = mcp_config(&source, &opencode, &codex);

        sync_mcp(&tmp, &config);

        assert_eq!(fs::read_to_string(&codex).unwrap(), original);
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_sync_mcp_null_mcp_servers_writes_empty_object() {
        let tmp = test_temp_dir("mcp-null");
        let source = tmp.join("mcp-servers.json");
        fs::write(&source, r#"{"mcpServers": null}"#).unwrap();
        let opencode = tmp.join("opencode/mcp.json");
        let codex = tmp.join("codex/config.toml");
        let config = mcp_config(&source, &opencode, &codex);

        sync_mcp(&tmp, &config);

        let oc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&opencode).unwrap()).unwrap();
        assert_eq!(oc["mcpServers"], serde_json::json!({}));
        fs::remove_dir_all(&tmp).ok();
    }

    /// Expected: abort with a hard error so a typo cannot retarget the default
    /// `~/skills` tree. Observed: invalid TOML is warned about and the empty
    /// default config is used instead.
    #[test]
    fn documents_defect_invalid_config_silently_uses_defaults() {
        let tmp = test_temp_dir("cfg-invalid");
        write_config_file(&tmp, "this is not { valid toml");
        let cfg = load_config_from(&tmp);
        assert!(
            cfg.skills.is_none() && cfg.mcp.is_none() && cfg.ce.is_none(),
            "current fail-open: malformed config becomes Config::default()"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    /// Expected: `targets = []` means "sync nowhere". Observed: an empty
    /// list is treated as missing and the four default target paths are used.
    #[test]
    fn documents_defect_empty_targets_fall_back_to_defaults() {
        let home = PathBuf::from("/home/user");
        let config = test_config_from_toml("[skills]\ntargets = []\n");
        let resolved = targets(&config, &home);
        assert_eq!(
            resolved.iter().map(|t| t.path.clone()).collect::<Vec<_>>(),
            vec![
                PathBuf::from("/home/user/.claude/skills"),
                PathBuf::from("/home/user/.opencode/skills"),
                PathBuf::from("/home/user/.codex/skills"),
                PathBuf::from("/home/user/.agents/skills"),
            ]
        );
    }

    /// Expected: an empty `source` is rejected (or treated as missing so the
    /// default `~/skills` applies). Observed: `expand_tilde("")` yields an
    /// empty path, which `read_dir` interprets as the process cwd.
    #[test]
    fn documents_defect_empty_source_path_is_cwd() {
        let home = PathBuf::from("/home/user");
        let config = test_config_from_toml("[skills]\nsource = \"\"\n");
        assert_eq!(skills_source(&config, &home), PathBuf::from(""));
    }

    /// Expected: missing skills source is a graceful error / empty listing,
    /// including for `--check`. Observed: `sync_skills` panics.
    #[test]
    #[should_panic(expected = "cannot read skills source")]
    fn documents_defect_missing_skills_source_panics() {
        let tmp = test_temp_dir("missing-src");
        let source = tmp.join("skills");
        let target = tmp.join("target");
        let raw = format!("[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = []\n");
        let config = test_config_from_toml(&raw);
        let _ = sync_skills(&tmp, &config, true);
    }

    /// Expected: a real directory already living in a target is left
    /// untouched (the prune path documents this invariant). Observed: the
    /// link pass `remove_dir_all`s it and replaces it with a symlink.
    #[test]
    fn documents_defect_link_pass_deletes_real_target_directory() {
        let tmp = test_temp_dir("clobber-dir");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "alpha");
        let target = tmp.join("target");
        let real = target.join("alpha");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("precious.txt"), "do not delete").unwrap();
        let raw = format!("[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = []\n");
        let config = test_config_from_toml(&raw);

        sync_skills(&tmp, &config, false);

        let dest = target.join("alpha");
        assert!(
            dest.is_symlink(),
            "current behaviour: real directory is replaced with a symlink"
        );
        assert!(
            !dest.join("precious.txt").exists(),
            "current behaviour: directory contents are destroyed"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    /// Expected: a live foreign symlink in the target (destination outside
    /// the skills source) is never removed. Observed: the link pass unlinks
    /// it and plants a source-derived symlink of the same name.
    #[test]
    fn documents_defect_link_pass_replaces_foreign_symlink() {
        let tmp = test_temp_dir("clobber-foreign");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "alpha");
        let outside = tmp.join("elsewhere");
        fs::create_dir_all(&outside).unwrap();
        let target = tmp.join("target");
        fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&outside, target.join("alpha")).unwrap();
        let raw = format!("[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = []\n");
        let config = test_config_from_toml(&raw);

        sync_skills(&tmp, &config, false);

        assert_eq!(
            fs::read_link(target.join("alpha")).unwrap(),
            source.join("alpha"),
            "current behaviour: foreign symlink is overwritten with a source link"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    /// Expected: only dangling *managed* links (those that resolved into the
    /// skills source) are cleaned up. Observed: every dangling symlink in a
    /// target is deleted, including foreign ones.
    #[test]
    fn documents_defect_dangling_foreign_symlink_is_deleted() {
        let tmp = test_temp_dir("dangling-foreign");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "alpha");
        let target = tmp.join("target");
        fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(tmp.join("does-not-exist"), target.join("ghost")).unwrap();
        let raw = format!("[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = []\n");
        let config = test_config_from_toml(&raw);

        sync_skills(&tmp, &config, false);

        assert!(
            target.join("ghost").symlink_metadata().is_err(),
            "current behaviour: dangling foreign symlink is removed"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    /// Expected: README "Symlinks and entries in `skip` are ignored" — a
    /// symlink in the source tree is not a skill. Observed: `Path::is_dir`
    /// follows the symlink, so a link to a skill directory is synced too.
    #[test]
    fn documents_defect_source_symlink_is_treated_as_skill() {
        let tmp = test_temp_dir("src-symlink");
        let source = tmp.join("skills");
        fs::create_dir_all(&source).unwrap();
        test_write_skill(&source, "alpha");
        std::os::unix::fs::symlink(source.join("alpha"), source.join("alias")).unwrap();
        let target = tmp.join("target");
        let raw = format!("[skills]\nsource = {source:?}\ntargets = [{target:?}]\nskip = []\n");
        let config = test_config_from_toml(&raw);

        let count = sync_skills(&tmp, &config, false);

        assert_eq!(
            count, 2,
            "current behaviour: source symlink counts as a skill"
        );
        assert!(target.join("alias").is_symlink());
        fs::remove_dir_all(&tmp).ok();
    }

    /// Expected: rewrite of OpenCode mcp.json preserves sibling keys such as
    /// `$schema`. Observed: the file is replaced with an object that only
    /// contains `mcpServers`.
    #[test]
    fn documents_defect_opencode_write_drops_sibling_keys() {
        let tmp = test_temp_dir("oc-clobber");
        let source = tmp.join("mcp-servers.json");
        fs::write(&source, r#"{"mcpServers": {"fs": {"command": "npx"}}}"#).unwrap();
        let opencode = tmp.join("opencode/mcp.json");
        fs::create_dir_all(opencode.parent().unwrap()).unwrap();
        fs::write(
            &opencode,
            r#"{"$schema":"https://example.com/schema.json","mcpServers":{}}"#,
        )
        .unwrap();
        let codex = tmp.join("codex/config.toml");
        let config = mcp_config(&source, &opencode, &codex);

        sync_mcp(&tmp, &config);

        let oc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&opencode).unwrap()).unwrap();
        assert!(
            oc.get("$schema").is_none(),
            "current behaviour: sibling keys are dropped"
        );
        assert_eq!(oc["mcpServers"]["fs"]["command"], "npx");
        fs::remove_dir_all(&tmp).ok();
    }

    /// Expected: a Codex managed-block error rolls back or skips the whole
    /// MCP step. Observed: OpenCode mcp.json is written first, then the
    /// function returns, leaving the two consumers out of sync.
    #[test]
    fn documents_defect_unclosed_codex_block_still_writes_opencode() {
        let tmp = test_temp_dir("partial-mcp");
        let source = tmp.join("mcp-servers.json");
        fs::write(
            &source,
            r#"{"mcpServers": {"fs": {"command": "npx"}}, "_codexExtras": {"x": {"url": "https://x.example"}}}"#,
        )
        .unwrap();
        let opencode = tmp.join("opencode/mcp.json");
        let codex = tmp.join("codex/config.toml");
        fs::create_dir_all(codex.parent().unwrap()).unwrap();
        fs::write(&codex, format!("{MCP_BEGIN}\nno end\n")).unwrap();
        let config = mcp_config(&source, &opencode, &codex);

        sync_mcp(&tmp, &config);

        assert!(
            opencode.exists(),
            "current behaviour: OpenCode file is written before the Codex error return"
        );
        fs::remove_dir_all(&tmp).ok();
    }
}
