//! Text helpers shared by all user-facing outputs (API, chat, events).

/// First line, without absolute paths, at most 200 characters.
pub fn user_message(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let cleaned: Vec<String> = line
        .split(' ')
        .map(|word| {
            let trimmed = word.trim_matches(|c| matches!(c, '\'' | '"' | ',' | ':' | '(' | ')'));
            if trimmed.starts_with('/') && trimmed.len() > 1 {
                let name = trimmed.rsplit('/').next().unwrap_or("");
                word.replace(trimmed, &format!("…/{name}"))
            } else {
                word.to_owned()
            }
        })
        .collect();
    let mut message = cleaned.join(" ");
    if message.chars().count() > 200 {
        message = message.chars().take(199).collect::<String>() + "…";
    }
    if message.is_empty() {
        "the request failed".into()
    } else {
        message
    }
}

/// Neutralises markup in untrusted text (track titles from websites) by replacing square
/// brackets with look-alikes, so `[img]`/`[url]` BBCode is shown instead of interpreted.
pub fn escape_markup(text: &str) -> String {
    text.replace('[', "\u{FF3B}").replace(']', "\u{FF3D}")
}

#[cfg(test)]
mod tests {
    use super::user_message;

    #[test]
    fn markup_is_neutralised() {
        assert_eq!(
            super::escape_markup("[img]http://x[/img]"),
            "［img］http://x［/img］"
        );
    }

    #[test]
    fn hides_paths_and_extra_lines() {
        assert_eq!(
            user_message("'/srv/james/data/media/a.mp3' is not available\nffprobe: details"),
            "'…/a.mp3' is not available"
        );
        assert_eq!(user_message(&"x".repeat(500)).chars().count(), 200);
    }
}
