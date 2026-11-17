#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(zeroclaw_tools::task_ssh_broker::run(
        std::env::args().skip(1).collect(),
    ));
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("{}", zeroclaw_tools::unsupported_task_ssh_message());
    std::process::exit(255);
}
