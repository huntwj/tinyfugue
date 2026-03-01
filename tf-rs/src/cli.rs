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
    /// `--install-libs [<dir>]`: extract embedded lib files and exit.
    /// `Some(None)` = use OS default data dir; `Some(Some(p))` = explicit dir.
    pub install_libs: Option<Option<PathBuf>>,
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

/// Where to load library files from.
pub enum LibSource {
    /// Load from this directory on disk (explicit or auto-detected).
    Path(PathBuf),
    /// No usable directory found on disk; use embedded files.
    /// The inner `PathBuf` is the OS data dir that `--install-libs` would write to,
    /// used as the `%TFLIBDIR` hint so that user-installed overrides are found there.
    Embedded(PathBuf),
}

impl LibSource {
    /// The path reported as `%TFLIBDIR` inside TF.
    pub fn as_path(&self) -> &PathBuf {
        match self {
            LibSource::Path(p) | LibSource::Embedded(p) => p,
        }
    }
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

        // Long options (`--foo` or `--foo <val>`).
        if arg.starts_with("--") {
            match arg {
                "--install-libs" => {
                    let dir = argv
                        .get(i + 1)
                        .filter(|a| !a.starts_with('-'))
                        .map(|a| {
                            i += 1;
                            PathBuf::from(a)
                        });
                    args.install_libs = Some(dir);
                }
                other => return Err(format!("illegal option -- -{}", &other[2..])),
            }
            i += 1;
            continue;
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
                    // Multiple -c flags accumulate, separated by %;
                    args.command = Some(match args.command.take() {
                        None => cmd,
                        Some(prev) => format!("{prev}%;{cmd}"),
                    });
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

                c => return Err(format!("illegal option -- {c}")),
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

/// Return the OS-appropriate user data directory for TF library files.
///
/// - Linux:   `~/.local/share/tf`
/// - macOS:   `~/Library/Application Support/tf`
/// - Windows: `%APPDATA%\tf`
///
/// Falls back to `~/.local/share/tf` if the `directories` crate cannot
/// determine the platform data dir.
pub fn default_user_tf_dir() -> PathBuf {
    use directories::ProjectDirs;
    ProjectDirs::from("", "", "tf")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(format!("{home}/.local/share/tf"))
        })
}

/// Determine where to load library files from.
///
/// Priority:
/// 1. `-L<dir>` CLI flag — use that dir on disk
/// 2. `$TFLIBDIR` env var — use that dir on disk
/// 3. `CARGO_MANIFEST_DIR/../lib/tf` — repo lib dir (dev builds via `cargo run`)
/// 4. OS user data dir (`~/.local/share/tf` on Linux) — if `stdlib.tf` is present
/// 5. Embedded — use files baked into the binary
pub fn resolve_libdir(cli_override: Option<&PathBuf>) -> LibSource {
    // 1. Explicit -L flag.
    if let Some(d) = cli_override {
        return LibSource::Path(d.clone());
    }

    // 2. TFLIBDIR env var.
    if let Ok(d) = std::env::var("TFLIBDIR") {
        return LibSource::Path(PathBuf::from(d));
    }

    // 3. Development: repo's lib/tf alongside the workspace root.
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let dev = PathBuf::from(&manifest)
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("lib/tf");
        if dev.exists() {
            return LibSource::Path(dev);
        }
    }

    // 4. OS user data dir — only if stdlib.tf is actually installed there.
    let user_dir = default_user_tf_dir();
    if user_dir.join("stdlib.tf").exists() {
        return LibSource::Path(user_dir);
    }

    // 5. Fall back to embedded files; report the user data dir as %TFLIBDIR so
    //    that files placed there by `--install-libs` are picked up automatically.
    LibSource::Embedded(user_dir)
}

/// Write all embedded lib files to `dest`, creating the directory if needed.
///
/// Existing files are skipped (preserves user customisations).
/// Returns the number of files newly written.
pub fn install_embedded_libs(dest: &PathBuf) -> Result<usize, String> {
    std::fs::create_dir_all(dest)
        .map_err(|e| format!("cannot create {}: {e}", dest.display()))?;

    let mut count = 0;
    for (name, content) in crate::embedded::all_embedded() {
        let path = dest.join(name);
        if path.exists() {
            continue; // preserve customisation
        }
        std::fs::write(&path, content)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        count += 1;
    }
    Ok(count)
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

    #[test]
    fn install_libs_no_dir() {
        let a = parse_argv(&argv(&["--install-libs"])).unwrap();
        assert!(matches!(a.install_libs, Some(None)));
    }

    #[test]
    fn install_libs_with_dir() {
        let a = parse_argv(&argv(&["--install-libs", "/tmp/mylibs"])).unwrap();
        assert!(
            matches!(&a.install_libs, Some(Some(p)) if p == &PathBuf::from("/tmp/mylibs"))
        );
    }

    #[test]
    fn install_libs_embedded_get() {
        // stdlib.tf must always be present in the embedded registry.
        let src = crate::embedded::get_embedded("stdlib.tf");
        assert!(src.is_some());
        assert!(src.unwrap().contains("/def"));
    }

    #[test]
    fn install_libs_writes_files() {
        let dir = tempfile::tempdir().unwrap();
        let count = install_embedded_libs(&dir.path().to_path_buf()).unwrap();
        assert!(count > 0);
        assert!(dir.path().join("stdlib.tf").exists());
    }

    #[test]
    fn install_libs_skips_existing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("stdlib.tf"), b"custom").unwrap();
        install_embedded_libs(&dir.path().to_path_buf()).unwrap();
        // Custom file should not be overwritten.
        let content = std::fs::read(dir.path().join("stdlib.tf")).unwrap();
        assert_eq!(content, b"custom");
    }
}
