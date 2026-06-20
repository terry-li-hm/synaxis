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
    targets: Option<Vec<String>>,
    skip: Option<Vec<String>>,
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
    let home = home();
    let path = config_path(&home);
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

fn skills_source(config: &Config, home: &Path) -> PathBuf {
    config
        .skills
        .as_ref()
        .and_then(|s| s.source.as_deref())
        .map(|s| expand_tilde(s, home))
        .unwrap_or_else(|| home.join("skills"))
}

fn targets(config: &Config, home: &Path) -> Vec<PathBuf> {
    if let Some(paths) = config
        .skills
        .as_ref()
        .and_then(|s| s.targets.as_ref())
        .filter(|paths| !paths.is_empty())
    {
        return paths.iter().map(|p| expand_tilde(p, home)).collect();
    }
    vec![
        home.join(".claude/skills"),
        home.join(".opencode/skills"),
        home.join(".codex/skills"),
        home.join(".agents/skills"),
    ]
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

    let mut active_targets = Vec::new();
    if !dry_run {
        for t in &targets {
            if t == &skills_dir {
                eprintln!(
                    "⚠  Skills: target {} is the source directory; skipping",
                    t.display()
                );
                continue;
            }
            if t.is_symlink() {
                if let Err(err) = fs::remove_file(t) {
                    eprintln!(
                        "⚠  Skills: cannot replace target symlink {} ({})",
                        t.display(),
                        err
                    );
                    continue;
                }
            }
            if let Err(err) = fs::create_dir_all(t) {
                eprintln!("⚠  Skills: cannot create target {} ({})", t.display(), err);
                continue;
            }
            active_targets.push(t.clone());
        }
        for t in &active_targets {
            if let Ok(entries) = fs::read_dir(t) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_symlink() && !path.exists() {
                        fs::remove_file(&path).ok();
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
                    let dest = t.join(name.as_ref());
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
        mcp_servers: parsed.mcp_servers.clone(),
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
            let end = start + end_rel + MCP_END.len();
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
        assert_eq!(
            targets(&config, &home),
            vec![
                PathBuf::from("/home/user/.claude/skills"),
                PathBuf::from("/home/user/.opencode/skills"),
                PathBuf::from("/home/user/.codex/skills"),
                PathBuf::from("/home/user/.agents/skills"),
            ]
        );
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
            vec!["codex".to_string(), "opencode".to_string(), "gemini".to_string()]
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
        assert_eq!(result.matches(MCP_BEGIN).count(), 1, "exactly one begin marker");
        assert_eq!(result.matches(MCP_END).count(), 1, "exactly one end marker");
        assert!(result.contains("new"));
        assert!(result.contains("middle"));
        assert!(!result.contains("old1"));
        assert!(!result.contains("old2"));
    }

    #[test]
    fn test_replace_or_append_managed_block_errors_on_unclosed_begin() {
        let existing = format!("{}\nno end marker here", MCP_BEGIN);
        let block = format!("{}\nnew\n{}\n", MCP_BEGIN, MCP_END);
        assert!(replace_or_append_managed_block(&existing, &block).is_err());
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
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let home = home();
    let cfg_path = config_path(&home);

    if args.iter().any(|a| a == "--help" || a == "-h") {
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

    if args.iter().any(|a| a == "--init") {
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

    let full = args.iter().any(|a| a == "--full");
    let check = args.iter().any(|a| a == "--check");
    let config = load_config();

    if check {
        println!("Skills in {}:", skills_source(&config, &home).display());
        let count = sync_skills(&home, &config, true);
        println!("{} skills", count);
        exit(0);
    }

    let count = sync_skills(&home, &config, false);
    eprintln!("✓  Skills: {} synced", count);

    if full {
        sync_mcp(&home, &config);
        sync_ce(&home, &config);
        eprintln!("Done. Restart Codex/OpenCode to pick up changes.");
    }
}
