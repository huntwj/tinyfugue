use tf::cli::{self, ConfigFile, ConnectTarget};
use tf::event_loop::EventLoop;
use tf::script::value::Value;

#[tokio::main]
async fn main() {
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

    // ── Set TFLIBDIR in the interpreter ───────────────────────────────────────
    let libdir = cli::resolve_libdir(args.libdir.as_ref());
    event_loop
        .interp
        .set_global_var("TFLIBDIR", Value::Str(libdir.display().to_string()));

    let tflibrary = libdir.join("stdlib.tf");
    event_loop
        .interp
        .set_global_var("TFLIBRARY", Value::Str(tflibrary.display().to_string()));

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
    }

    // ── Enter main loop ───────────────────────────────────────────────────────
    if let Err(e) = event_loop.run().await {
        eprintln!("tf: {e}");
        std::process::exit(1);
    }
}
