fn main() {
    if let Err(e) = gqls::cli::run() {
        // A handled outcome has already said its piece (candidates to pick
        // from); all that's left is a status a script can act on.
        if !e.is::<gqls::cli::Handled>() {
            eprintln!("gqls: {e:#}");
        }
        std::process::exit(1);
    }
}
