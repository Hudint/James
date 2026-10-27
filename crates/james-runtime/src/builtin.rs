//! Built-in chat commands.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail};
use async_trait::async_trait;

use crate::commands::{Command, CommandContext, CommandRegistry};
use crate::instance::PlayMode;
use crate::player::{QueueItem, RepeatMode, Requester};

type Output<'a> = Pin<Box<dyn Future<Output = anyhow::Result<Option<String>>> + Send + 'a>>;
type Handler = for<'a> fn(&'a CommandContext) -> Output<'a>;

/// Permission names used by the built-in commands.
pub mod permissions {
    pub const PLAY: &str = "playback.play";
    pub const CONTROL: &str = "playback.control";
    pub const QUEUE: &str = "queue.manage";
    pub const VOLUME: &str = "playback.volume";
}

struct Builtin {
    name: &'static str,
    aliases: &'static [&'static str],
    usage: &'static str,
    description: &'static str,
    permission: Option<&'static str>,
    handler: Handler,
}

#[async_trait]
impl Command for Builtin {
    fn name(&self) -> &str {
        self.name
    }
    fn aliases(&self) -> &[&str] {
        self.aliases
    }
    fn usage(&self) -> &str {
        self.usage
    }
    fn description(&self) -> &str {
        self.description
    }
    fn permission(&self) -> Option<&str> {
        self.permission
    }
    async fn run(&self, ctx: &CommandContext) -> anyhow::Result<Option<String>> {
        (self.handler)(ctx).await
    }
}

pub fn register(registry: &CommandRegistry) {
    use permissions::*;
    let commands: Vec<Builtin> = vec![
        Builtin {
            name: "help",
            aliases: &["h", "commands"],
            usage: "[command]",
            description: "Lists commands or shows help for one command",
            permission: None,
            handler: help,
        },
        Builtin {
            name: "play",
            aliases: &["p"],
            usage: "<url|search|path>",
            description: "Plays a track immediately",
            permission: Some(PLAY),
            handler: play,
        },
        Builtin {
            name: "add",
            aliases: &["a", "enqueue"],
            usage: "<url|search|path>",
            description: "Adds tracks to the end of the queue",
            permission: Some(PLAY),
            handler: add,
        },
        Builtin {
            name: "playnext",
            aliases: &["pn"],
            usage: "<url|search|path>",
            description: "Queues tracks to play next",
            permission: Some(PLAY),
            handler: playnext,
        },
        Builtin {
            name: "skip",
            aliases: &["next", "n", "s"],
            usage: "",
            description: "Skips to the next track",
            permission: Some(CONTROL),
            handler: skip,
        },
        Builtin {
            name: "prev",
            aliases: &["previous", "back"],
            usage: "",
            description: "Plays the previous track",
            permission: Some(CONTROL),
            handler: prev,
        },
        Builtin {
            name: "stop",
            aliases: &[],
            usage: "",
            description: "Stops playback",
            permission: Some(CONTROL),
            handler: stop,
        },
        Builtin {
            name: "pause",
            aliases: &[],
            usage: "",
            description: "Pauses playback",
            permission: Some(CONTROL),
            handler: pause,
        },
        Builtin {
            name: "resume",
            aliases: &["continue", "unpause"],
            usage: "",
            description: "Resumes playback",
            permission: Some(CONTROL),
            handler: resume,
        },
        Builtin {
            name: "volume",
            aliases: &["vol", "v"],
            usage: "[0-100|+n|-n]",
            description: "Shows or sets the volume",
            permission: Some(VOLUME),
            handler: volume,
        },
        Builtin {
            name: "nowplaying",
            aliases: &["np", "song", "current"],
            usage: "",
            description: "Shows the current track",
            permission: None,
            handler: nowplaying,
        },
        Builtin {
            name: "queue",
            aliases: &["q", "list"],
            usage: "[page]",
            description: "Shows the queue",
            permission: None,
            handler: queue,
        },
        Builtin {
            name: "clear",
            aliases: &[],
            usage: "",
            description: "Clears the queue",
            permission: Some(QUEUE),
            handler: clear,
        },
        Builtin {
            name: "remove",
            aliases: &["rm"],
            usage: "<position>",
            description: "Removes a track from the queue",
            permission: Some(QUEUE),
            handler: remove,
        },
        Builtin {
            name: "move",
            aliases: &["mv"],
            usage: "<from> <to>",
            description: "Moves a track within the queue",
            permission: Some(QUEUE),
            handler: move_item,
        },
        Builtin {
            name: "shuffle",
            aliases: &[],
            usage: "[on|off]",
            description: "Shuffles the queue once, or toggles shuffle mode",
            permission: Some(QUEUE),
            handler: shuffle,
        },
        Builtin {
            name: "repeat",
            aliases: &["loop"],
            usage: "[off|one|all]",
            description: "Shows or sets the repeat mode",
            permission: Some(CONTROL),
            handler: repeat,
        },
        Builtin {
            name: "seek",
            aliases: &[],
            usage: "<m:ss|+s|-s>",
            description: "Jumps to a position in the current track",
            permission: Some(CONTROL),
            handler: seek,
        },
    ];
    for command in commands {
        registry.register(Arc::new(command));
    }
}

fn requester(ctx: &CommandContext) -> Option<Requester> {
    Some(Requester {
        uid: ctx.invoker.uid.clone(),
        name: ctx.invoker.name.clone(),
    })
}

pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// Parses `90`, `1:30`, `1:02:03`.
pub fn parse_duration(input: &str) -> Option<Duration> {
    let mut secs = 0u64;
    for part in input.split(':') {
        secs = secs * 60 + part.trim().parse::<u64>().ok()?;
    }
    Some(Duration::from_secs(secs))
}

fn describe(item: &QueueItem) -> String {
    let mut text = match &item.track.artist {
        Some(artist) => format!("{} – {}", artist, item.track.title),
        None => item.track.title.clone(),
    };
    if let Some(d) = item.track.duration {
        text.push_str(&format!(" ({})", format_duration(d)));
    }
    text
}

fn added_message(items: &[QueueItem], verb: &str) -> String {
    match items {
        [] => "Nothing added.".into(),
        [one] => format!("{verb}: {}", describe(one)),
        [first, rest @ ..] => format!("{verb}: {} and {} more", describe(first), rest.len()),
    }
}

fn require_args(ctx: &CommandContext) -> anyhow::Result<&str> {
    if ctx.args.is_empty() {
        bail!("missing argument");
    }
    Ok(&ctx.args)
}

fn help(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        let registry = &ctx.instance.services().commands;
        if !ctx.args.is_empty() {
            let command = registry
                .get(&ctx.args)
                .ok_or_else(|| anyhow!("unknown command '{}'", ctx.args))?;
            let mut text = format!(
                "{} {} – {}",
                command.name(),
                command.usage(),
                command.description()
            );
            if !command.aliases().is_empty() {
                text.push_str(&format!(" (aliases: {})", command.aliases().join(", ")));
            }
            return Ok(Some(text));
        }
        let lines: Vec<String> = registry
            .list()
            .iter()
            .map(|c| format!("{} {} – {}", c.name(), c.usage(), c.description()))
            .collect();
        Ok(Some(format!("Commands:\n{}", lines.join("\n"))))
    })
}

fn play_with<'a>(ctx: &'a CommandContext, mode: PlayMode, verb: &'static str) -> Output<'a> {
    Box::pin(async move {
        let input = require_args(ctx)?;
        let items = ctx.instance.play(input, mode, requester(ctx)).await?;
        Ok(Some(added_message(&items, verb)))
    })
}

fn play(ctx: &CommandContext) -> Output<'_> {
    play_with(ctx, PlayMode::Now, "Playing")
}

fn add(ctx: &CommandContext) -> Output<'_> {
    play_with(ctx, PlayMode::Append, "Queued")
}

fn playnext(ctx: &CommandContext) -> Output<'_> {
    play_with(ctx, PlayMode::Next, "Up next")
}

fn skip(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        ctx.instance.skip()?;
        Ok(None)
    })
}

fn prev(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        ctx.instance.previous()?;
        Ok(None)
    })
}

fn stop(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        ctx.instance.stop()?;
        Ok(None)
    })
}

fn pause(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        ctx.instance.pause(true)?;
        Ok(None)
    })
}

fn resume(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        ctx.instance.pause(false)?;
        Ok(None)
    })
}

fn volume(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        let current = ctx.instance.status().await?.volume;
        if ctx.args.is_empty() {
            return Ok(Some(format!("Volume: {current}")));
        }
        let arg = ctx.args.as_str();
        let value: i32 = if let Some(delta) = arg.strip_prefix('+') {
            i32::from(current) + delta.parse::<i32>()?
        } else if let Some(delta) = arg.strip_prefix('-') {
            i32::from(current) - delta.parse::<i32>()?
        } else {
            arg.parse()?
        };
        let value = value.clamp(0, 100) as u8;
        ctx.instance.set_volume(value)?;
        Ok(Some(format!("Volume: {value}")))
    })
}

fn nowplaying(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        let status = ctx.instance.status().await?;
        Ok(Some(match status.current {
            Some(item) => {
                let position = format_duration(Duration::from_millis(status.position_ms));
                let mut text = format!("Now playing: {} [{position}]", describe(&item));
                if let Some(by) = item.requested_by {
                    text.push_str(&format!(", requested by {}", by.name));
                }
                if status.paused {
                    text.push_str(" (paused)");
                }
                text
            }
            None => "Nothing is playing.".into(),
        }))
    })
}

const PAGE_SIZE: usize = 10;

fn queue(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        let items = ctx.instance.queue().await?;
        if items.is_empty() {
            return Ok(Some("The queue is empty.".into()));
        }
        let pages = items.len().div_ceil(PAGE_SIZE);
        let page = ctx.args.parse::<usize>().unwrap_or(1).clamp(1, pages);
        let lines: Vec<String> = items
            .iter()
            .enumerate()
            .skip((page - 1) * PAGE_SIZE)
            .take(PAGE_SIZE)
            .map(|(i, item)| format!("{}. {}", i + 1, describe(item)))
            .collect();
        Ok(Some(format!(
            "Queue ({} tracks, page {page}/{pages}):\n{}",
            items.len(),
            lines.join("\n")
        )))
    })
}

fn clear(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        ctx.instance.clear_queue()?;
        Ok(Some("Queue cleared.".into()))
    })
}

fn position_arg(arg: &str) -> anyhow::Result<usize> {
    let n: usize = arg
        .parse()
        .map_err(|_| anyhow!("'{arg}' is not a queue position"))?;
    n.checked_sub(1).ok_or_else(|| anyhow!("positions start at 1"))
}

fn remove(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        let item = ctx.instance.remove(position_arg(require_args(ctx)?)?).await?;
        Ok(Some(format!("Removed: {}", describe(&item))))
    })
}

fn move_item(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        let mut parts = ctx.args.split_whitespace();
        let (Some(from), Some(to)) = (parts.next(), parts.next()) else {
            bail!("usage: move <from> <to>")
        };
        ctx.instance
            .move_item(position_arg(from)?, position_arg(to)?)
            .await?;
        Ok(Some("Moved.".into()))
    })
}

fn shuffle(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        match ctx.args.to_ascii_lowercase().as_str() {
            "" => {
                ctx.instance.shuffle_queue()?;
                Ok(Some("Queue shuffled.".into()))
            }
            "on" | "true" | "1" => {
                ctx.instance.set_shuffle(true)?;
                Ok(Some("Shuffle on.".into()))
            }
            "off" | "false" | "0" => {
                ctx.instance.set_shuffle(false)?;
                Ok(Some("Shuffle off.".into()))
            }
            other => bail!("unknown option '{other}'"),
        }
    })
}

fn repeat(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        let mode = match ctx.args.to_ascii_lowercase().as_str() {
            "" => {
                let status = ctx.instance.status().await?;
                return Ok(Some(format!("Repeat: {:?}", status.repeat).to_lowercase()));
            }
            "off" | "none" => RepeatMode::Off,
            "one" | "track" | "single" => RepeatMode::One,
            "all" | "queue" => RepeatMode::All,
            other => bail!("unknown repeat mode '{other}'"),
        };
        ctx.instance.set_repeat(mode)?;
        Ok(Some(format!("Repeat: {mode:?}").to_lowercase()))
    })
}

fn seek(ctx: &CommandContext) -> Output<'_> {
    Box::pin(async move {
        let arg = require_args(ctx)?;
        let status = ctx.instance.status().await?;
        let current = status.current.ok_or_else(|| anyhow!("nothing is playing"))?;
        if current.track.is_stream {
            bail!("live streams cannot be seeked");
        }
        let position = Duration::from_millis(status.position_ms);
        let target = if let Some(delta) = arg.strip_prefix('+') {
            position + parse_duration(delta).ok_or_else(|| anyhow!("invalid time"))?
        } else if let Some(delta) = arg.strip_prefix('-') {
            position.saturating_sub(parse_duration(delta).ok_or_else(|| anyhow!("invalid time"))?)
        } else {
            parse_duration(arg).ok_or_else(|| anyhow!("invalid time '{arg}'"))?
        };
        ctx.instance.seek(target)?;
        Ok(Some(format!("Seeked to {}", format_duration(target))))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("1:30"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("1:02:03"), Some(Duration::from_secs(3723)));
        assert_eq!(parse_duration("x"), None);
        assert_eq!(format_duration(Duration::from_secs(3723)), "1:02:03");
        assert_eq!(format_duration(Duration::from_secs(65)), "1:05");
    }
}
