//! Command-line argument parsing.
//!
//! Usage:
//!   tf [-L<dir>] [-f[<file>]] [-c<cmd>] [-vnlqd] [<world>]
//!   tf [-L<dir>] [-f[<file>]] [-c<cmd>] [-vlqd]  <host> <port>

use std::path::PathBuf;

// ── Public types ──────────────────────────────────────────────────────────────

/// Parsed command-line arguments.
#[derive(Debug, Default)]
pub struct CliArgs {
    /// Library directory override (`-L<dir>`).
    pub libdir: Option<PathBuf>,
    /// Config-file specification.
    pub config: ConfigFile,
    /// Command to execute after loading config (`-c<cmd>`).
    pub command: Option<String>,
    /// Disable automatic first connection (`-n`).
    pub no_connect: bool,
    /// Disable autologin (`-l`).
    pub no_autologin: bool,
    /// Quiet login (`-q`).
    pub quiet_login: bool,
    /// Disable visual mode (`-v`).
    pub no_visual: bool,
    /// Debug mode (`-d`).
    pub debug: bool,
    /// What to connect to on startup.
    pub connect: ConnectTarget,
}

/// How to choose the user config file.
#[derive(Debug, Default)]
pub enum ConfigFile {
    /// Search `~/.tfrc`, `~/tfrc`, `./.tfrc`, `./tfrc` in order (default).
    #[default]
    Search,
    /// `-f` with no file argument: skip user config.
    Skip,
    /// `-f<file>`: load this specific file.
    Explicit(PathBuf),
}

/// What to connect to on startup.
#[derive(Debug, Default)]
pub enum ConnectTarget {
    /// Connect to the default world (no positional args).
    #[default]
    Default,
    /// Connect to a named world.
    World(String),
    /// Connect to a raw host:port (unnamed world).
    HostPort(String, u16),
}

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Parse `std::env::args()` and return [`CliArgs`] or an error message.
pub fn parse_args() -> Result<CliArgs, String> {
    let raw: Vec<String> = std::env::args().collect();
    parse_argv(&raw[1..])
}

/// Parse a slice of argument strings (exposed for testing).
pub fn parse_argv(argv: &[String]) -> Result<CliArgs, String> {
    let mut args = CliArgs::default();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;

    while i < argv.len() {
        let arg = argv[i].as_str();

        // `--` ends flag processing.
        if arg == "--" {
            i += 1;
            positional.extend(argv[i..].iter().cloned());
            break;
        }

        // Non-flag argument.
        if !arg.starts_with('-') || arg == "-" {
            positional.push(arg.to_owned());
            i += 1;
            continue;
        }

        // Flag argument: iterate over characters after the leading `-`.
        let chars: Vec<char> = arg[1..].chars().collect();
        let mut j = 0;
        while j < chars.len() {
            match chars[j] {
                'd' => args.debug = true,
                'l' => args.no_autologin = true,
                'q' => args.quiet_login = true,
                'n' => args.no_connect = true,
                'v' => args.no_visual = true,

                // -f[<file>]
                'f' => {
                    if j + 1 < chars.len() {
                        // Embedded: -f<file>
                        let file: String = chars[j + 1..].iter().collect();
                        args.config = ConfigFile::Explicit(PathBuf::from(file));
                        j = chars.len(); // consumed rest of this arg
                    } else if i + 1 < argv.len() && !argv[i + 1].starts_with('-') {
                        // Separate: -f <file>
                        i += 1;
                        args.config = ConfigFile::Explicit(PathBuf::from(&argv[i]));
                    } else {
                        // -f alone → skip user config
                        args.config = ConfigFile::Skip;
                    }
                }

                // -c<cmd>
                'c' => {
                    let cmd = if j + 1 < chars.len() {
                        let s: String = chars[j + 1..].iter().collect();
                        j = chars.len();
                        s
                    } else if i + 1 < argv.len() {
                        i += 1;
                        argv[i].clone()
                    } else {
                        return Err("-c requires a command argument".to_owned());
                    };
                    args.command = Some(cmd);
                }

                // -L<dir>
                'L' => {
                    let dir = if j + 1 < chars.len() {
                        let s: String = chars[j + 1..].iter().collect();
                        j = chars.len();
                        s
                    } else if i + 1 < argv.len() {
                        i += 1;
                        argv[i].clone()
                    } else {
                        return Err("-L requires a directory argument".to_owned());
                    };
                    args.libdir = Some(PathBuf::from(dir));
                }

                c => return Err(format!("unknown option: -{c}")),
            }
            j += 1;
        }
        i += 1;
    }

    // Positional arguments → connect target.
    match positional.len() {
        0 => {}
        1 => args.connect = ConnectTarget::World(positional.remove(0)),
        2 => {
            let host = positional.remove(0);
            let port: u16 = positional[0]
                .parse()
                .map_err(|_| format!("invalid port number: {}", positional[0]))?;
            args.connect = ConnectTarget::HostPort(host, port);
        }
        n => return Err(format!("too many arguments ({n})")),
    }

    Ok(args)
}

// ── Path helpers ──────────────────────────────────────────────────────────────

/// Search for the user config file in the standard locations.
/// Returns the first path that exists, or `None`.
pub fn find_user_config() -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    [
        format!("{home}/.tfrc"),
        format!("{home}/tfrc"),
        "./.tfrc".to_owned(),
        "./tfrc".to_owned(),
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

/// Determine the TF library directory.
///
/// Priority: `-L<dir>` CLI flag → `TFLIBDIR` env var → path relative to the
/// binary (development/installed layout) → `/usr/local/lib/tf`.
pub fn resolve_libdir(cli_override: Option<&PathBuf>) -> PathBuf {
    if let Some(d) = cli_override {
        return d.clone();
    }
    if let Ok(d) = std::env::var("TFLIBDIR") {
        return PathBuf::from(d);
    }
    // During development, look alongside the workspace root.
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let dev = PathBuf::from(&manifest)
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("lib/tf");
        if dev.exists() {
            return dev;
        }
    }
    PathBuf::from("/usr/local/lib/tf")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|&s| s.to_owned()).collect()
    }

    #[test]
    fn empty_args() {
        let a = parse_argv(&argv(&[])).unwrap();
        assert!(!a.no_connect);
        assert!(matches!(a.connect, ConnectTarget::Default));
    }

    #[test]
    fn world_positional() {
        let a = parse_argv(&argv(&["mymud"])).unwrap();
        assert!(matches!(&a.connect, ConnectTarget::World(w) if w == "mymud"));
    }

    #[test]
    fn host_port_positional() {
        let a = parse_argv(&argv(&["mud.example.com", "4000"])).unwrap();
        assert!(
            matches!(&a.connect, ConnectTarget::HostPort(h, 4000) if h == "mud.example.com")
        );
    }

    #[test]
    fn bool_flags() {
        let a = parse_argv(&argv(&["-l", "-q", "-n", "-v", "-d"])).unwrap();
        assert!(a.no_autologin);
        assert!(a.quiet_login);
        assert!(a.no_connect);
        assert!(a.no_visual);
        assert!(a.debug);
    }

    #[test]
    fn combined_bool_flags() {
        let a = parse_argv(&argv(&["-lqnvd"])).unwrap();
        assert!(a.no_autologin && a.quiet_login && a.no_connect && a.no_visual && a.debug);
    }

    #[test]
    fn libdir_embedded() {
        let a = parse_argv(&argv(&["-L/some/dir"])).unwrap();
        assert_eq!(a.libdir, Some(PathBuf::from("/some/dir")));
    }

    #[test]
    fn libdir_separate() {
        let a = parse_argv(&argv(&["-L", "/some/dir"])).unwrap();
        assert_eq!(a.libdir, Some(PathBuf::from("/some/dir")));
    }

    #[test]
    fn config_skip() {
        let a = parse_argv(&argv(&["-f"])).unwrap();
        assert!(matches!(a.config, ConfigFile::Skip));
    }

    #[test]
    fn config_explicit_embedded() {
        let a = parse_argv(&argv(&["-fmyrc.tf"])).unwrap();
        assert!(matches!(&a.config, ConfigFile::Explicit(p) if p == &PathBuf::from("myrc.tf")));
    }

    #[test]
    fn config_explicit_separate() {
        let a = parse_argv(&argv(&["-f", "myrc.tf"])).unwrap();
        assert!(matches!(&a.config, ConfigFile::Explicit(p) if p == &PathBuf::from("myrc.tf")));
    }

    #[test]
    fn command_embedded() {
        let a = parse_argv(&argv(&["-c/echo hello"])).unwrap();
        assert_eq!(a.command.as_deref(), Some("/echo hello"));
    }

    #[test]
    fn too_many_positional() {
        assert!(parse_argv(&argv(&["a", "b", "c"])).is_err());
    }

    #[test]
    fn unknown_flag() {
        assert!(parse_argv(&argv(&["-z"])).is_err());
    }
}
