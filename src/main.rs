fn main() {
    if let Err(error) = dynws::run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
