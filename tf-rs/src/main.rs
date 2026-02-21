use tf::event_loop::EventLoop;

#[tokio::main]
async fn main() {
    // TODO: parse CLI args and load config (Phase 10).
    let mut event_loop = EventLoop::new();
    if let Err(e) = event_loop.run().await {
        eprintln!("tf: {e}");
        std::process::exit(1);
    }
}
