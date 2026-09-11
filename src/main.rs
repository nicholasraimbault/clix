fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if let Err(err) = clix::parse_argv(&argv) {
        eprintln!("{err}");
        std::process::exit(2);
    }
}
