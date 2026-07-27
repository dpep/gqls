fn main() {
    if let Err(e) = gqls::cli::run() {
        eprintln!("gqls: {e:#}");
        std::process::exit(1);
    }
}
