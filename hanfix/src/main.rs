fn main() {
    #[cfg(windows)]
    {
        use hanfix::win;
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.iter().any(|a| a == "--debug") {
            win::DEBUG.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if args.iter().any(|a| a == "--selftest-target") {
            std::process::exit(win::selftest::target());
        }
        if args.iter().any(|a| a == "--ime-status") {
            win::ime_status(args.iter().any(|a| a == "--fix"));
            return;
        }
        if args.iter().any(|a| a == "--diag-inject") {
            win::inject_f24(5);
            return;
        }
        let help = args.iter().any(|a| a == "--help" || a == "-h");
        if !help && !win::claim_single_instance() {
            eprintln!("hanfix is already running (or a selftest is). Quit it first: Ctrl+C in its console window.");
            win::pause_if_own_console();
            std::process::exit(4);
        }
        if args.iter().any(|a| a == "--diag-hook") {
            win::diag_hook();
            return;
        }
        if args.iter().any(|a| a == "--selftest") {
            let code = win::selftest::run();
            println!("\nresults saved to {}", win::data_dir().join("selftest.txt").display());
            win::pause_if_own_console();
            std::process::exit(code);
        }
        if args.iter().any(|a| a == "--help" || a == "-h") {
            println!("hanfix — fixes words typed in the wrong 한/영 mode when you press Space\n");
            println!("  hanfix             run (Ctrl+Alt+F12 pauses, Ctrl+C quits)");
            println!("  hanfix --selftest  check this PC's IME in a private test window (enables English fixes if supported)");
            println!("\nsettings: {}", win::Config::path().display());
            return;
        }
        win::run(win::Config::load());
    }
    #[cfg(not(windows))]
    eprintln!("hanfix runs on Windows only; the engine is available as a library.");
}
