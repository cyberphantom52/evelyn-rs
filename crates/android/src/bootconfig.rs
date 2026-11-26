//! A vendor_boot image's bootconfig: `key=value` lines, which the kernel gives
//! init as `ro.boot.*` properties for `androidboot.*` keys.

fn key(line: &str) -> Option<&str> {
    line.split_once('=').map(|(key, _)| key.trim())
}

/// `text` with every line that has `key` set to `value`, or a line added.
pub fn set(text: &str, key_: &str, value: &str) -> String {
    let line = format!("{key_}={value}");
    let mut found = false;
    let mut lines: Vec<String> = text
        .lines()
        .map(|old| match key(old) == Some(key_) {
            true => {
                found = true;
                line.clone()
            }
            false => old.to_owned(),
        })
        .collect();
    if !found {
        lines.push(line);
    }
    lines.join("\n") + "\n"
}

/// `text` without the lines that have `key`; `None` if none does.
pub fn remove(text: &str, key_: &str) -> Option<String> {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| key(line) != Some(key_))
        .collect();
    (lines.len() != text.lines().count()).then(|| lines.join("\n") + "\n")
}
