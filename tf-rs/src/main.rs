use tf::cli::{self, ConfigFile, ConnectTarget};
use tf::event_loop::EventLoop;
use tf::hook::Hook;
use tf::script::builtins::tf_features_string;
use tf::script::value::Value;

#[tokio::main]
async fn main() {
    // ── Early stdout banner (mirrors C TF's puts() calls before arg parsing) ─
    // These appear before the TF UI starts, even when stdout is redirected.
    let ver = env!("CARGO_PKG_VERSION");
    println!();
    println!("TinyFugue (tf) version {ver} (Rust rewrite)");
    println!("Copyright (C) 1993-2007 Ken Keys.  Rust rewrite (C) 2024-2025 contributors.");

    let args = match cli::parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("tf: {e}");
            eprintln!(
                "Usage: tf [-L<dir>] [-f[<file>]] [-c<cmd>] [-vnlqd] [<world>]"
            );
            eprintln!(
                "       tf [-L<dir>] [-f[<file>]] [-c<cmd>] [-vlqd]  <host> <port>"
            );
            std::process::exit(1);
        }
    };

    let mut event_loop = EventLoop::new();

    // ── Set built-in interpreter globals ──────────────────────────────────────
    event_loop
        .interp
        .set_global_var("version", Value::Str(env!("CARGO_PKG_VERSION").to_owned()));
    event_loop
        .interp
        .set_global_var("features", Value::Str(tf_features_string()));

    // ── Set TFLIBDIR in the interpreter ───────────────────────────────────────
    let libdir = cli::resolve_libdir(args.libdir.as_ref());
    event_loop
        .interp
        .set_global_var("TFLIBDIR", Value::Str(libdir.display().to_string()));

    let tflibrary = libdir.join("stdlib.tf");
    event_loop
        .interp
        .set_global_var("TFLIBRARY", Value::Str(tflibrary.display().to_string()));

    // ── Set variable defaults (mirrors C TF's init_variables / varlist.h) ────
    // These are read by stdlib.tf and user scripts; set sensible defaults so
    // they exist even if the user never assigns them.
    for (name, val) in [
        ("quiet",  Value::Int(if args.quiet_login { 1 } else { 0 })),
        ("gag",    Value::Int(0)),
        ("hilite", Value::Int(1)),
        ("scroll", Value::Int(1)),
        ("wrap",   Value::Int(1)),
        ("login",  Value::Int(1)),
        ("sub",    Value::Str("both".to_owned())),
        ("more",   Value::Int(1)),
    ] {
        event_loop.interp.set_global_var(name, val);
    }

    // ── Load stdlib.tf (required — fatal if missing) ──────────────────────────
    if let Err(e) = event_loop.load_script_file(&tflibrary) {
        eprintln!("tf: Can't read required library: {e}");
        std::process::exit(1);
    }

    // ── Load user config ──────────────────────────────────────────────────────
    match args.config {
        ConfigFile::Skip => {} // -f alone: skip user config
        ConfigFile::Explicit(path) => {
            if let Err(e) = event_loop.load_script_file(&path) {
                eprintln!("tf: warning: {e}");
            }
        }
        ConfigFile::Search => {
            if let Some(path) = cli::find_user_config() {
                if let Err(e) = event_loop.load_script_file(&path) {
                    eprintln!("tf: warning: {e}");
                }
            }
        }
    }

    // ── Set %visual and %interactive (mirrors C TF main.c:201-207) ──────────
    // C sets these after config loading, only if not explicitly set (< 0).
    // We always set them here; a user config loaded above may have overridden
    // them by the time we reach this point, but stdlib.tf reads them on connect
    // so they must be present with sensible defaults.
    let is_tty = unsafe {
        libc::isatty(libc::STDIN_FILENO) != 0 && libc::isatty(libc::STDOUT_FILENO) != 0
    };
    event_loop.interp.set_global_var(
        "interactive",
        Value::Int(if is_tty { 1 } else { 0 }),
    );
    event_loop.interp.set_global_var(
        "visual",
        Value::Int(if is_tty && !args.no_visual { 1 } else { 0 }),
    );

    // ── Execute startup command (-c<cmd>) ─────────────────────────────────────
    if let Some(cmd) = args.command {
        if let Err(e) = event_loop.interp.exec_script(&cmd) {
            eprintln!("tf: {e}");
        }
        for line in event_loop.interp.output.drain(..) {
            println!("{line}");
        }
        event_loop.interp.take_actions(); // discard startup actions
    }

    // ── Startup banner (mirrors C TF's oputs() calls in main.c) ─────────────
    let ver = env!("CARGO_PKG_VERSION");
    event_loop.push_output(&format!("TinyFugue (tf) version {ver} (Rust rewrite)"));
    event_loop.push_output(
        "Copyright (C) 1993-2007 Ken Keys.  \
         Rust rewrite (C) 2024-2025 project contributors."
    );
    event_loop.push_output("Type `/help', `/help topics', or `/help intro' for help.");
    event_loop.push_output("Type `/quit' to quit tf.");
    event_loop.push_output("");

    // ── Auto-connect ──────────────────────────────────────────────────────────
    if !args.no_connect {
        match args.connect {
            ConnectTarget::Default => {
                // Always attempt (matching C TF behaviour); connect_world_by_name
                // will display "% Unknown world ''" if no default world is defined.
                event_loop.connect_world_by_name("").await;
            }
            ConnectTarget::World(name) => {
                event_loop.connect_world_by_name(&name).await;
            }
            ConnectTarget::HostPort(host, port) => {
                let name = format!("{host}:{port}");
                if let Err(e) = event_loop.connect(&name, &host, port).await {
                    eprintln!("tf: connect {host}:{port}: {e}");
                }
            }
        }
    } else {
        // -n flag: no automatic connection (mirrors C TF main.c:221-222).
        let msg = "---- No world ----";
        event_loop.push_output(msg);
        event_loop.fire_hook(Hook::World, msg).await;
    }

    // ── Enter main loop ───────────────────────────────────────────────────────
    if let Err(e) = event_loop.run().await {
        eprintln!("tf: {e}");
        std::process::exit(1);
    }
}
