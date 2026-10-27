//! Shared media helpers.

use std::path::Path;

/// File extensions treated as audio files (local library, uploads, direct stream URLs).
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "m4a", "aac", "wav", "wma", "alac", "aiff", "ape", "wv", "mka",
    "webm",
];

/// ffmpeg demuxers allowed for local files (`-format_whitelist`). Playlist-like formats
/// (HLS, DASH, concat, …) are excluded: their content could make ffmpeg fetch other URLs,
/// no matter what the file extension claims.
pub const LOCAL_DEMUXERS: &str = "mp3,flac,ogg,wav,w64,aac,aiff,ape,wv,asf,caf,matroska,mov";

/// ffmpeg/ffprobe input arguments for a local file: only the `file` protocol, only audio
/// container formats, and the `file:` prefix so names like `concat:x` are not protocols.
pub fn local_input_args(path: &Path) -> Vec<String> {
    vec![
        "-protocol_whitelist".into(),
        "file".into(),
        "-format_whitelist".into(),
        LOCAL_DEMUXERS.into(),
        "-i".into(),
        format!("file:{}", path.display()),
    ]
}

/// ffmpeg input arguments for media piped from a helper process: only the `pipe` protocol
/// and only audio container formats.
pub fn pipe_input_args() -> Vec<String> {
    [
        "-protocol_whitelist",
        "pipe",
        "-format_whitelist",
        LOCAL_DEMUXERS,
        "-i",
        "pipe:0",
    ]
    .map(String::from)
    .to_vec()
}

pub fn is_audio_extension(ext: &str) -> bool {
    AUDIO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

pub fn is_audio_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(is_audio_extension)
}
