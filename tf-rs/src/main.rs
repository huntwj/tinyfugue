use tf::cli::{self, ConfigFile, ConnectTarget, LibSource};
use tf::event_loop::EventLoop;
use tf::hook::Hook;
use tf::script::builtins::tf_features_string;
use tf::script::value::Value;

#[tokio::main(flavor = "current_thread")]
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
            eprintln!("\ntf: {e}");
            eprintln!();
            eprintln!("Usage: tf [-L<dir>] [-f[<file>]] [-c<cmd>] [-vnlqd] [<world>]");
            eprintln!("       tf [-L<dir>] [-f[<file>]] [-c<cmd>] [-vlqd]  <host> <port>");
            eprintln!("Options:");
            eprintln!("  -L<dir>   use <dir> as library directory (%TFLIBDIR)");
            eprintln!("  -f        don't load personal config file (.tfrc)");
            eprintln!("  -f<file>  load <file> instead of config file");
            eprintln!("  -c<cmd>   execute <cmd> after loading config file");
            eprintln!("  -n        no automatic first connection");
            eprintln!("  -l        no automatic login/password");
            eprintln!("  -q        quiet login");
            eprintln!("  -v        no automatic visual mode");
            eprintln!("  -d        debug mode");
            eprintln!("Arguments:");
            eprintln!("  <host>    hostname or IP address");
            eprintln!("  <port>    port number or name");
            eprintln!("  <world>   connect to <world> defined by addworld()");
            eprintln!();
            std::process::exit(1);
        }
    };

    // ── --install-libs: extract embedded files and exit ───────────────────────
    if let Some(dest) = args.install_libs {
        let dir = dest.unwrap_or_else(cli::default_user_tf_dir);
        match cli::install_embedded_libs(&dir) {
            Ok(0) => println!("All files already present in {} (nothing written).", dir.display()),
            Ok(n) => println!("Installed {n} file(s) to {}.", dir.display()),
            Err(e) => {
                eprintln!("tf: --install-libs: {e}");
                std::process::exit(1);
            }
        }
        std::process::exit(0);
    }

    let mut event_loop = EventLoop::new();

    // ── Thread CLI flags through to event loop ────────────────────────────────
    event_loop.no_autologin = args.no_autologin;
    event_loop.quiet_login  = args.quiet_login;

    // ── Set built-in interpreter globals ──────────────────────────────────────
    event_loop
        .interp
        .set_global_var("version", Value::Str(env!("CARGO_PKG_VERSION").to_owned()));
    event_loop
        .interp
        .set_global_var("features", Value::Str(tf_features_string()));

    // ── Resolve lib source and set TFLIBDIR / TFLIBRARY ──────────────────────
    let lib_source = cli::resolve_libdir(args.libdir.as_ref());
    let libdir_str = lib_source.as_path().display().to_string();
    event_loop
        .interp
        .set_global_var("TFLIBDIR", Value::Str(libdir_str.clone()));
    event_loop
        .interp
        .set_global_var("TFLIBRARY", Value::Str(format!("{libdir_str}/stdlib.tf")));

    // ── Set variable defaults (mirrors C TF's init_variables / varlist.h) ────
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
    let stdlib_result = match &lib_source {
        LibSource::Path(dir) => event_loop.load_script_file(&dir.join("stdlib.tf")),
        LibSource::Embedded(_) => {
            let src = tf::embedded::get_embedded("stdlib.tf")
                .expect("stdlib.tf is always embedded");
            event_loop.load_script_source(src, "stdlib.tf")
        }
    };
    if let Err(e) = stdlib_result {
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
