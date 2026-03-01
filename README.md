# synaxis

Keep your AI coding tools in sync — skills, MCP servers, and compound-engineering across Claude Code, OpenCode, Codex, and Gemini CLI from a single source of truth.

```
$ synaxis --full
✓  Skills: 64 synced
✓  MCP: wrote OpenCode config
✓  MCP: wrote Codex config block
✓  CE: compound-engineering installed to codex
✓  CE: compound-engineering installed to opencode
✓  CE: compound-engineering installed to gemini
Done. Restart Codex/OpenCode to pick up changes.
```

## What it syncs

| Step | What | Source → Targets |
|------|------|-----------------|
| Skills | Symlinks | `~/skills/` → `~/.claude/skills/`, `~/.opencode/skills/`, `~/.codex/skills/`, `~/.agents/skills/` |
| MCP | Config files | `mcp-servers.json` → `~/.opencode/mcp.json` (JSON) + `~/.codex/config.toml` (managed TOML block) |
| CE | Plugin install | `every-marketplace` → codex, opencode, gemini via `bunx @every-env/compound-plugin` |

Gemini CLI has no skills directory — CE only.

## Install

```bash
cargo install synaxis
```

Or build from source:

```bash
git clone https://github.com/terry-li-hm/synaxis
cd synaxis
cargo build --release
# add ./target/release/synaxis to your PATH
```

## Usage

```
synaxis              # Sync skills only (fast, ~50ms)
synaxis --full       # Skills + MCP + compound-engineering
synaxis --check      # Dry run — list skills, no changes
synaxis --init       # Create default config at ~/.config/synaxis/config.toml
synaxis --help
```

**Skills-only is the default** because it's fast enough to fire on every git commit. Use `--full` after changing MCP config or updating compound-engineering.

## Auto-sync on commit

Wire synaxis into your skills repo so symlinks stay fresh automatically:

```bash
# .git/hooks/post-commit
#!/bin/bash
synaxis
```

```bash
chmod +x .git/hooks/post-commit
```

## Config

Run `synaxis --init` to create `~/.config/synaxis/config.toml` with defaults:

```toml
[skills]
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
```

All fields are optional — synaxis falls back to the defaults above if the config file is absent or a field is missing.

## MCP source format

`mcp-servers.json` uses a split key design: `mcpServers` propagates to OpenCode (JSON), while `_codexExtras` propagates to Codex (TOML). This lets you keep OpenCode's HTTP-based servers separate from Codex's URL-based entries without duplicating config.

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path"]
    }
  },
  "_codexExtras": {
    "context7": { "url": "https://mcp.context7.com/mcp" }
  }
}
```

The Codex TOML block is written between `# synaxis-mcp-begin` / `# synaxis-mcp-end` markers, so it coexists safely with manually maintained config.

## Skills discovery

A directory under `source` is treated as a skill if it contains a `SKILL.md` file. Symlinks and entries in `skip` are ignored. Each skill is symlinked by name into every target directory.

> **Codex gotcha:** dir-level symlinks break skill discovery in Codex ([#11314](https://github.com/openai/codex/issues/11314)). synaxis works around this by creating per-skill symlinks inside a real directory rather than a single directory-level symlink.

## License

MIT
