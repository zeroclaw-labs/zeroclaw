fn normalize_domain(raw: &str) -> Option<String> {
    let mut d = raw.trim().to_lowercase();
    if d.is_empty() { return None; }
    if let Some(stripped) = d.strip_prefix("https://") { d = stripped.to_string(); } else if let Some(stripped) = d.strip_prefix("http://") { d = stripped.to_string(); }
    if let Some((host, _)) = d.split_once('/') { d = host.to_string(); }
    d = d.trim_start_matches('.').trim_end_matches('.').to_string();
    if let Some((host, _)) = d.split_once(':') { d = host.to_string(); }
    if d.is_empty() || d.chars().any(char::is_whitespace) { return None; }
    Some(d)
}

fn main() {
    println!("{:?}", normalize_domain("*"));
}
