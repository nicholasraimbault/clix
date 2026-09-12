fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let cmd = match clix::parse_argv(&argv) {
        Ok(cmd) => cmd,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(2);
        }
    };
    if let Err(err) = clix::dispatch(cmd) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
