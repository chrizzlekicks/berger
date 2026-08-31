mod event;
mod fs_util;
mod hook;
mod init;
mod name;
mod reconcile;
mod reset;
mod state;
mod sync;
mod tmux;

fn usage() -> ! {
    eprintln!("usage: berger <init|event|reset|sync --session <name>>");
    std::process::exit(1);
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("init") => init::run(),
        Some("event") => event::run(),
        Some("reset") => reset::run(),
        Some("sync") => {
            let mut session = None;
            while let Some(arg) = args.next() {
                if arg == "--session" {
                    session = args.next();
                } else {
                    eprintln!("berger sync: unknown argument '{arg}'");
                    std::process::exit(1);
                }
            }
            match session {
                Some(s) => sync::run(&s),
                None => {
                    eprintln!("berger sync: --session <name> is required");
                    std::process::exit(1);
                }
            }
        }
        Some(other) => {
            eprintln!("berger: unknown command '{other}'");
            usage();
        }
        None => usage(),
    }
}
