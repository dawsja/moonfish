fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (cmd, tag) = (args.get(1).map(String::as_str), args.get(2).map(String::as_str).unwrap_or(""));
    if cmd == Some("--version") {
        println!("moonfish-host {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    #[cfg(unix)]
    let result = match cmd {
        Some("proxy") => moonfish_host::proxy::proxy(tag),
        Some("serve") => moonfish_host::daemon::serve(tag),
        _ => {
            eprintln!("usage: moonfish-host proxy|serve <tag>");
            std::process::exit(2);
        }
    };
    #[cfg(not(unix))]
    let result: std::io::Result<()> = {
        let _ = (cmd, tag);
        Err(std::io::Error::other("moonfish-host runs on Linux and macOS hosts"))
    };
    if let Err(e) = result {
        eprintln!("moonfish-host: {e}");
        std::process::exit(1);
    }
}
