//! `.tfrc` configuration file parser.
//!
//! Recognises the subset of TF script needed to load world definitions and
//! variable settings:
//!
//! | Directive | Action |
//! |-----------|--------|
//! | `/addworld [-pxe] [-T<type>] [-s<host>] <name> …` | add/update a world |
//! | `/set <name>=<value>` or `/set <name> <value>` | set a variable |
//! | Lines starting with `;` | comment, ignored |
//! | Any other `/command` | silently skipped |
//!
//! Full TF scripting (macros, conditionals, expressions) is Phase 4.

use std::path::Path;

use crate::var::VarStore;
use crate::world::{World, WorldFlags, WorldStore};

// ── Public API ────────────────────────────────────────────────────────────────

/// A non-fatal error encountered while loading a config file.
#[derive(Debug)]
pub struct ConfigError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ConfigError {}

/// Parsed TF configuration: world definitions and variable settings.
#[derive(Debug, Default)]
pub struct Config {
    pub worlds: WorldStore,
    pub vars: VarStore,
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse a TF config string.
    ///
    /// Unknown directives are silently skipped so that a real `.tfrc` (which
    /// contains macros, triggers, etc.) can be loaded without error.
    /// Returns the config and a list of any parse errors on recognised lines.
    pub fn load_str(s: &str) -> (Self, Vec<ConfigError>) {
        let mut config = Config::new();
        let mut errors = Vec::new();

        for (i, raw) in s.lines().enumerate() {
            let lineno = i + 1;
            let line = raw.trim();

            // blank lines and comments (`;` or `;;` prefix)
            if line.is_empty() || line.starts_with(';') {
                continue;
            }

            // TF commands all begin with `/`
            let Some(rest) = line.strip_prefix('/') else { continue };

            // split off the command name
            let (cmd, args_str) = rest
                .split_once(|c: char| c.is_ascii_whitespace())
                .unwrap_or((rest, ""));
            let args_str = args_str.trim();

            match cmd {
                "addworld" => {
                    let tokens = split_args(args_str);
                    match parse_addworld(&tokens) {
                        Ok(world) => { config.worlds.upsert(world); }
                        Err(msg) => errors.push(ConfigError { line: lineno, message: msg }),
                    }
                }
                "set" => {
                    let tokens = split_args(args_str);
                    if let Err(msg) = parse_set(&tokens, &mut config.vars) {
                        errors.push(ConfigError { line: lineno, message: msg });
                    }
                }
                _ => {} // silently skip unknown commands (macros, /def, etc.)
            }
        }

        (config, errors)
    }

    /// Read and parse a TF config file from disk.
    pub fn load_file(path: &Path) -> std::io::Result<(Self, Vec<ConfigError>)> {
        let s = std::fs::read_to_string(path)?;
        Ok(Self::load_str(&s))
    }
}

// ── Argument tokenizer ────────────────────────────────────────────────────────

/// Split `s` into whitespace-delimited tokens, honouring double-quoted strings
/// and `\"` escapes within them.
fn split_args(s: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = s.chars();

    while let Some(ch) = chars.next() {
        match ch {
            '"' if !in_quotes => in_quotes = true,
            '"' if in_quotes => in_quotes = false,
            '\\' if in_quotes => {
                if let Some(escaped) = chars.next() {
                    cur.push(escaped);
                }
            }
            c if c.is_ascii_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        args.push(cur);
    }
    args
}

// ── /addworld ─────────────────────────────────────────────────────────────────

struct AddWorldOpts {
    flags: WorldFlags,
    world_type: Option<String>,
    myhost: Option<String>,
    /// Positional (non-flag) arguments.
    positional: Vec<String>,
}

/// Parse `/addworld` flags and positional args.
///
/// Recognised flags (matching `stdlib.tf`'s `getopts("pxeT:s:", "")`):
/// `-p` no-proxy, `-x` SSL, `-e` echo, `-T<type>`, `-s<host>`.
/// Flags may be combined (`-pxe`) and value flags may be attached (`-Ttiny`)
/// or separated (`-T tiny`).
fn parse_opts(tokens: &[String]) -> AddWorldOpts {
    let mut flags = WorldFlags::default();
    let mut world_type: Option<String> = None;
    let mut myhost: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];

        if tok.starts_with('-') && tok.len() > 1 {
            let mut chars = tok[1..].chars();
            while let Some(ch) = chars.next() {
                match ch {
                    'p' => flags.no_proxy = true,
                    'x' => flags.ssl = true,
                    'e' => flags.echo = true,
                    'T' | 's' => {
                        // value is the rest of this token, or the next token
                        let inline: String = chars.collect();
                        let value = if !inline.is_empty() {
                            inline
                        } else {
                            i += 1;
                            match tokens.get(i) {
                                Some(v) => v.clone(),
                                None => break,
                            }
                        };
                        if ch == 'T' {
                            world_type = Some(value);
                        } else {
                            myhost = Some(value);
                        }
                        break; // rest of token consumed by value
                    }
                    _ => {} // ignore unrecognised flags
                }
            }
        } else {
            positional.push(tok.clone());
        }

        i += 1;
    }

    AddWorldOpts { flags, world_type, myhost, positional }
}

/// Build a [`World`] from parsed `/addworld` options.
///
/// Positional argument forms (matching `stdlib.tf`):
///
/// Named worlds:
/// - `<name>`
/// - `<name> <host> <port>`
/// - `<name> <host> <port> <mfile>`
/// - `<name> <char> <pass> <host> <port>`
/// - `<name> <char> <pass> <host> <port> <mfile>`
///
/// Default world (`name` = `"default"` case-insensitive):
/// - `default`
/// - `default <char>`
/// - `default <char> <pass>`
/// - `default <char> <pass> <mfile>`
fn parse_addworld(tokens: &[String]) -> Result<World, String> {
    let opts = parse_opts(tokens);
    let pos = &opts.positional;

    if pos.is_empty() {
        return Err("addworld: requires at least a world name".into());
    }

    let mut w = World::named(pos[0].clone());
    w.flags = opts.flags;
    w.world_type = opts.world_type;
    w.myhost = opts.myhost;

    if w.name.eq_ignore_ascii_case("default") {
        match pos.len() {
            1 => {}
            2 => w.character = Some(pos[1].clone()),
            3 => {
                w.character = Some(pos[1].clone());
                w.pass = Some(pos[2].clone());
            }
            _ => {
                w.character = Some(pos[1].clone());
                w.pass = Some(pos[2].clone());
                w.mfile = Some(pos[3].clone());
            }
        }
    } else {
        match pos.len() {
            1 => {}
            3 => {
                w.host = Some(pos[1].clone());
                w.port = Some(pos[2].clone());
            }
            4 => {
                w.host = Some(pos[1].clone());
                w.port = Some(pos[2].clone());
                w.mfile = Some(pos[3].clone());
            }
            5 => {
                w.character = Some(pos[1].clone());
                w.pass = Some(pos[2].clone());
                w.host = Some(pos[3].clone());
                w.port = Some(pos[4].clone());
            }
            6 => {
                w.character = Some(pos[1].clone());
                w.pass = Some(pos[2].clone());
                w.host = Some(pos[3].clone());
                w.port = Some(pos[4].clone());
                w.mfile = Some(pos[5].clone());
            }
            n => return Err(format!("addworld: unexpected {n} positional arguments")),
        }
    }

    Ok(w)
}

// ── /set ─────────────────────────────────────────────────────────────────────

/// Parse `/set <name>=<value>` or `/set <name> <value>`.
fn parse_set(tokens: &[String], vars: &mut VarStore) -> Result<(), String> {
    if tokens.is_empty() {
        return Err("/set: requires an argument".into());
    }

    let (name, value) = if let Some(eq) = tokens[0].find('=') {
        (tokens[0][..eq].to_owned(), tokens[0][eq + 1..].to_owned())
    } else if tokens.len() >= 2 {
        (tokens[0].clone(), tokens[1..].join(" "))
    } else {
        return Err(format!("/set: missing value for '{}'", tokens[0]));
    };

    if name.is_empty() {
        return Err("/set: variable name cannot be empty".into());
    }

    vars.set(name, value);
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // -- split_args -----------------------------------------------------------

    #[test]
    fn split_simple() {
        assert_eq!(split_args("foo bar baz"), ["foo", "bar", "baz"]);
    }

    #[test]
    fn split_quoted_spaces() {
        assert_eq!(split_args(r#""My MUD" 4242"#), ["My MUD", "4242"]);
    }

    #[test]
    fn split_escaped_quote_inside_quotes() {
        assert_eq!(split_args(r#""say \"hi\"""#), [r#"say "hi""#]);
    }

    // -- /addworld ------------------------------------------------------------

    #[test]
    fn addworld_name_host_port() {
        let (cfg, errs) = Config::load_str("/addworld Pax pax.mud.net 4321");
        assert!(errs.is_empty(), "{errs:?}");
        let w = cfg.worlds.find("Pax").unwrap();
        assert_eq!(w.host.as_deref(), Some("pax.mud.net"));
        assert_eq!(w.port.as_deref(), Some("4321"));
        assert!(w.character.is_none());
    }

    #[test]
    fn addworld_with_auth() {
        let (cfg, errs) = Config::load_str("/addworld Pax mychar mypass pax.mud.net 4321");
        assert!(errs.is_empty(), "{errs:?}");
        let w = cfg.worlds.find("Pax").unwrap();
        assert_eq!(w.character.as_deref(), Some("mychar"));
        assert_eq!(w.pass.as_deref(), Some("mypass"));
        assert_eq!(w.host.as_deref(), Some("pax.mud.net"));
        assert_eq!(w.port.as_deref(), Some("4321"));
    }

    #[test]
    fn addworld_with_mfile() {
        let (cfg, errs) = Config::load_str(
            "/addworld Pax mychar mypass pax.mud.net 4321 ~/.tf/pax.tf",
        );
        assert!(errs.is_empty(), "{errs:?}");
        let w = cfg.worlds.find("Pax").unwrap();
        assert_eq!(w.mfile.as_deref(), Some("~/.tf/pax.tf"));
    }

    #[test]
    fn addworld_ssl_flag() {
        let (cfg, errs) = Config::load_str("/addworld -x Pax pax.mud.net 4321");
        assert!(errs.is_empty(), "{errs:?}");
        assert!(cfg.worlds.find("Pax").unwrap().flags.ssl);
    }

    #[test]
    fn addworld_combined_flags() {
        let (cfg, errs) = Config::load_str("/addworld -pxe Pax pax.mud.net 4321");
        assert!(errs.is_empty(), "{errs:?}");
        let f = &cfg.worlds.find("Pax").unwrap().flags;
        assert!(f.ssl);
        assert!(f.no_proxy);
        assert!(f.echo);
    }

    #[test]
    fn addworld_type_attached() {
        let (cfg, errs) = Config::load_str("/addworld -Ttiny Avalon av.mud.net 23");
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(cfg.worlds.find("Avalon").unwrap().world_type.as_deref(), Some("tiny"));
    }

    #[test]
    fn addworld_type_separated() {
        let (cfg, errs) = Config::load_str("/addworld -T tiny Avalon av.mud.net 23");
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(cfg.worlds.find("Avalon").unwrap().world_type.as_deref(), Some("tiny"));
    }

    #[test]
    fn addworld_quoted_name() {
        let (cfg, errs) = Config::load_str(r#"/addworld "My World" host.example.com 4242"#);
        assert!(errs.is_empty(), "{errs:?}");
        assert!(cfg.worlds.find("My World").is_some());
    }

    #[test]
    fn addworld_default_world() {
        let (cfg, errs) = Config::load_str("/addworld default mychar mypass");
        assert!(errs.is_empty(), "{errs:?}");
        let d = cfg.worlds.default_world().unwrap();
        assert_eq!(d.character.as_deref(), Some("mychar"));
        assert_eq!(d.pass.as_deref(), Some("mypass"));
    }

    #[test]
    fn addworld_name_only() {
        let (cfg, errs) = Config::load_str("/addworld Placeholder");
        assert!(errs.is_empty(), "{errs:?}");
        let w = cfg.worlds.find("Placeholder").unwrap();
        assert!(!w.is_connectable());
    }

    #[test]
    fn addworld_bad_arg_count_is_error() {
        let (_, errs) = Config::load_str("/addworld BadWorld only_one_extra_arg");
        assert!(!errs.is_empty());
    }

    // -- /set -----------------------------------------------------------------

    #[test]
    fn set_equals_syntax() {
        let (cfg, errs) = Config::load_str("/set wrap=1");
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(cfg.vars.get("wrap"), Some("1"));
    }

    #[test]
    fn set_space_syntax() {
        let (cfg, errs) = Config::load_str("/set tabsize 8");
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(cfg.vars.get("tabsize"), Some("8"));
    }

    #[test]
    fn set_value_with_spaces() {
        let (cfg, errs) = Config::load_str("/set greeting hello world");
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(cfg.vars.get("greeting"), Some("hello world"));
    }

    // -- Comments & skipping --------------------------------------------------

    #[test]
    fn semicolon_comments_ignored() {
        let (cfg, errs) = Config::load_str(
            ";; This is a comment\n\
             ; Also a comment\n\
             /set real=yes",
        );
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(cfg.vars.get("real"), Some("yes"));
        assert_eq!(cfg.vars.len(), 1);
    }

    #[test]
    fn blank_lines_ignored() {
        let (cfg, errs) = Config::load_str("\n\n/set x=1\n\n");
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(cfg.vars.get("x"), Some("1"));
    }

    #[test]
    fn unknown_commands_silently_skipped() {
        let (cfg, errs) = Config::load_str(
            "/def -i mytrigger = /echo hi\n\
             /set loaded=yes",
        );
        assert!(errs.is_empty(), "{errs:?}");
        assert!(cfg.vars.contains("loaded"));
    }

    #[test]
    fn realistic_tfrc() {
        let src = "\
;; My TF config\n\
\n\
/addworld -x Pax mychar mypass pax.example.com 4321\n\
/addworld -Ttiny Avalon av.example.com 23\n\
\n\
/set wrap=1\n\
/set tabsize=8\n\
\n\
;; Macros (skipped in phase 3)\n\
/def -i mymacro = /echo hello\n\
";
        let (cfg, errs) = Config::load_str(src);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(cfg.worlds.len(), 2);
        assert!(cfg.worlds.find("Pax").unwrap().flags.ssl);
        assert_eq!(cfg.worlds.find("Avalon").unwrap().world_type.as_deref(), Some("tiny"));
        assert_eq!(cfg.vars.get("wrap"), Some("1"));
        assert_eq!(cfg.vars.get("tabsize"), Some("8"));
    }
}
