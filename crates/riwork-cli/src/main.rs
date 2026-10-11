fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match riwork_cli::run_headless(&args) {
        Ok(status) => std::process::exit(status),
        Err(error) => {
            if !error.is_empty() {
                eprintln!("riwork: {error}");
            }
            std::process::exit(2);
        }
    }
}
