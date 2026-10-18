//! torrential: search torrent indexes and download torrents, from a terminal or an agent.

mod daemon;
mod env;
mod output {
    pub mod format;
}
mod paths;
mod wire;

fn main() -> std::process::ExitCode {
    let env = env::Env::from_process();
    std::process::ExitCode::from(daemon::run(&env, std::env::args().any(|a| a == "--service")))
}
