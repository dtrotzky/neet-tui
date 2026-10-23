fn main() {
    // Behave like a normal CLI when piped into `head` etc.
    // SAFETY: called before any threads exist; restores the default disposition.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    if let Err(e) = neet::cli::run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
