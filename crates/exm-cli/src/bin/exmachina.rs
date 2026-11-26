//! Entry point for the `exmachina` command.
fn main() {
    if let Err(e) = exm_cli::run() {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}
