fn main() {
    if let Err(error) = sauerteig_client::capture::run() {
        eprintln!("Capture failed: {error}");
        std::process::exit(1);
    }
}
