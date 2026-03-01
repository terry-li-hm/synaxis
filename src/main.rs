use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, exit};

const SKIP: &[&str] = &["TEMPLATE.md", ".git", ".archive", ".cache", ".gitignore", "config.json"];
const MCP_BEGIN: &str = "# necto-mcp-begin";
const MCP_END: &str = "# necto-mcp-end";

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
    std::env::var("HOME").map(PathBuf::from).expect("$HOME not set")
}

fn targets(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".claude/skills"),
        home.join(".opencode/skills"),
        home.join(".codex/skills"),
        home.join(".agents/skills"),
    ]
}

fn sync_skills(home: &Path, dry_run: bool) -> usize {
    let skills_dir = home.join("skills");
    let targets = targets(home);

    if !dry_run {
        for t in &targets {
            fs::create_dir_all(t).ok();
        }
        for t in &targets {
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
        .expect("cannot read ~/skills/")
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let item = entry.path();
        let name = item.file_name().unwrap().to_string_lossy();
        if SKIP.iter().any(|s| *s == name.as_ref()) || item.is_symlink() {
            continue;
        }
        if item.is_dir() && item.join("SKILL.md").exists() {
            if dry_run {
                println!("  {}", name);
            } else {
                for t in &targets {
                    let dest = t.join(name.as_ref());
                    if dest.is_symlink() || dest.exists() {
                        fs::remove_file(&dest).ok();
                    }
                    std::os::unix::fs::symlink(&item, &dest).ok();
                }
            }
            count += 1;
        }
    }
    count
}

fn sync_mcp(home: &Path) {
    let source = home.join("agent-config/mcp-servers.json");
    let raw = match fs::read_to_string(&source) {
        Ok(content) => content,
        Err(_) => {
            eprintln!("⚠  MCP: cannot read ~/agent-config/mcp-servers.json, skipping");
            return;
        }
    };

    let parsed: McpServersFile = match serde_json::from_str(&raw) {
        Ok(data) => data,
        Err(_) => {
            eprintln!("⚠  MCP: invalid JSON in ~/agent-config/mcp-servers.json, skipping");
            return;
        }
    };

    let opencode_path = home.join(".opencode/mcp.json");
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

    let codex_path = home.join(".codex/config.toml");
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

fn build_codex_mcp_block(codex_extras: &BTreeMap<String, CodexExtra>) -> String {
    let mut out = String::new();
    out.push_str(MCP_BEGIN);
    out.push('\n');
    for (name, extra) in codex_extras {
        out.push_str(&format!("[mcp_servers.{}]\n", name));
        let url = serde_json::to_string(&extra.url).unwrap_or_else(|_| "\"\"".to_string());
        out.push_str(&format!("url = {}\n", url));
    }
    out.push_str(MCP_END);
    out.push('\n');
    out
}

fn replace_or_append_managed_block(existing: &str, block: &str) -> Result<String, &'static str> {
    if let Some(start) = existing.find(MCP_BEGIN) {
        if let Some(end_rel) = existing[start..].find(MCP_END) {
            let end = start + end_rel + MCP_END.len();
            let mut out = String::new();
            out.push_str(&existing[..start]);
            out.push_str(block);
            out.push_str(&existing[end..]);
            return Ok(out);
        }
        return Err("found '# necto-mcp-begin' without matching '# necto-mcp-end'; leaving Codex config unchanged");
    }

    let mut out = existing.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(block);
    Ok(out)
}

fn sync_ce(home: &Path) {
    let ce_dir = home.join(".claude/plugins/marketplaces/every-marketplace");
    if !ce_dir.exists() {
        eprintln!("⚠  CE: every-marketplace not installed, skipping");
        return;
    }

    for target in ["codex", "opencode", "gemini"] {
        let status = Command::new("bunx")
            .args([
                "@every-env/compound-plugin",
                "install",
                "compound-engineering",
                "--to",
                target,
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

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: synaxis [--full] [--check]");
        println!();
        println!("  (default)  Sync skills symlinks (fast, safe for git hooks)");
        println!("  --full     Skills + MCP + compound-engineering");
        println!("  --check    Dry run — list skills, no changes");
        exit(0);
    }

    let full = args.iter().any(|a| a == "--full");
    let check = args.iter().any(|a| a == "--check");
    let home = home();

    if check {
        println!("Skills in ~/skills/:");
        let count = sync_skills(&home, true);
        println!("{} skills", count);
        exit(0);
    }

    let count = sync_skills(&home, false);
    eprintln!("✓  Skills: {} synced", count);

    if full {
        sync_mcp(&home);
        sync_ce(&home);
        eprintln!("Done. Restart Codex/OpenCode to pick up changes.");
    }
}
