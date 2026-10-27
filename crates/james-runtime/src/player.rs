//! Queue and playback order (pure logic, no I/O).

use std::collections::VecDeque;

use james_core::source::Track;
use rand::Rng;
use serde::{Deserialize, Serialize};

/// Who requested a track.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Requester {
    pub uid: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    /// Unique within the instance, stable while the item exists.
    pub id: u64,
    pub track: Track,
    pub requested_by: Option<Requester>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    /// Repeat the current track.
    One,
    /// Re-append finished tracks to the end of the queue.
    All,
}

const HISTORY_LIMIT: usize = 100;

#[derive(Debug, Default)]
pub struct Player {
    queue: VecDeque<QueueItem>,
    history: VecDeque<QueueItem>,
    current: Option<QueueItem>,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    next_id: u64,
}

impl Player {
    fn item(&mut self, track: Track, requested_by: Option<Requester>) -> QueueItem {
        self.next_id += 1;
        QueueItem {
            id: self.next_id,
            track,
            requested_by,
        }
    }

    pub fn current(&self) -> Option<&QueueItem> {
        self.current.as_ref()
    }

    pub fn queue(&self) -> &VecDeque<QueueItem> {
        &self.queue
    }

    pub fn history(&self) -> &VecDeque<QueueItem> {
        &self.history
    }

    /// Appends tracks to the end of the queue.
    pub fn enqueue(&mut self, tracks: Vec<Track>, requested_by: Option<Requester>) -> Vec<QueueItem> {
        let items: Vec<_> = tracks
            .into_iter()
            .map(|t| self.item(t, requested_by.clone()))
            .collect();
        self.queue.extend(items.iter().cloned());
        items
    }

    /// Inserts tracks at the front of the queue (played next, in order).
    pub fn enqueue_next(&mut self, tracks: Vec<Track>, requested_by: Option<Requester>) -> Vec<QueueItem> {
        let items: Vec<_> = tracks
            .into_iter()
            .map(|t| self.item(t, requested_by.clone()))
            .collect();
        for item in items.iter().rev() {
            self.queue.push_front(item.clone());
        }
        items
    }

    /// Makes the first track current immediately; further tracks are queued next.
    pub fn play_now(&mut self, mut tracks: Vec<Track>, requested_by: Option<Requester>) -> Option<QueueItem> {
        if tracks.is_empty() {
            return None;
        }
        let rest = tracks.split_off(1);
        self.enqueue_next(rest, requested_by.clone());
        let item = self.item(tracks.remove(0), requested_by);
        self.retire_current();
        self.current = Some(item.clone());
        Some(item)
    }

    fn retire_current(&mut self) {
        if let Some(item) = self.current.take() {
            self.history.push_back(item);
            if self.history.len() > HISTORY_LIMIT {
                self.history.pop_front();
            }
        }
    }

    /// Advances after the current track ended naturally (honours repeat modes).
    pub fn advance(&mut self) -> Option<QueueItem> {
        match (self.repeat, &self.current) {
            (RepeatMode::One, Some(current)) => return Some(current.clone()),
            (RepeatMode::All, Some(current)) => {
                let again = self.item(current.track.clone(), current.requested_by.clone());
                self.queue.push_back(again);
            }
            _ => {}
        }
        self.skip()
    }

    /// Moves to the next queued track regardless of repeat-one.
    pub fn skip(&mut self) -> Option<QueueItem> {
        self.retire_current();
        let next = if self.shuffle && self.queue.len() > 1 {
            let index = rand::rng().random_range(0..self.queue.len());
            self.queue.remove(index)
        } else {
            self.queue.pop_front()
        };
        self.current = next.clone();
        next
    }

    /// Returns to the previously played track; the current one goes back to the queue front.
    pub fn previous(&mut self) -> Option<QueueItem> {
        let previous = self.history.pop_back()?;
        if let Some(current) = self.current.take() {
            self.queue.push_front(current);
        }
        self.current = Some(previous.clone());
        Some(previous)
    }

    /// Clears the current track (playback stopped); the queue is kept.
    pub fn stop(&mut self) {
        self.retire_current();
    }

    pub fn clear(&mut self) {
        self.queue.clear();
    }

    /// Position (0-based) of the entry with this id (0-based).
    fn position(&self, id: u64) -> Option<usize> {
        self.queue.iter().position(|i| i.id == id)
    }

    /// Removes the queue entry with the given item id. Ids (unlike positions) stay valid while
    /// other clients change the queue concurrently.
    pub fn remove(&mut self, id: u64) -> Option<QueueItem> {
        let index = self.position(id)?;
        self.queue.remove(index)
    }

    /// Moves the entry with the given id to position `to` (0-based, clamped).
    pub fn move_item(&mut self, id: u64, to: usize) -> bool {
        let Some(from) = self.position(id) else {
            return false;
        };
        let item = self.queue.remove(from).expect("position exists");
        let to = to.min(self.queue.len());
        self.queue.insert(to, item);
        true
    }

    /// Makes a queued entry current immediately (the previous one goes to the history).
    pub fn jump(&mut self, id: u64) -> Option<QueueItem> {
        let item = self.remove(id)?;
        self.retire_current();
        self.current = Some(item.clone());
        Some(item)
    }

    pub fn shuffle_queue(&mut self) {
        let mut rng = rand::rng();
        let items = self.queue.make_contiguous();
        for i in (1..items.len()).rev() {
            items.swap(i, rng.random_range(0..=i));
        }
    }

    /// The track that will play after the current one, if deterministic.
    pub fn peek_next(&self) -> Option<&QueueItem> {
        match self.repeat {
            RepeatMode::One => self.current.as_ref(),
            _ if self.shuffle => None,
            _ => self.queue.front(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str) -> Track {
        Track {
            source: "test".into(),
            locator: title.into(),
            title: title.into(),
            artist: None,
            album: None,
            duration: None,
            thumbnail: None,
            is_stream: false,
        }
    }

    fn titles(p: &Player) -> Vec<&str> {
        p.queue().iter().map(|i| i.track.title.as_str()).collect()
    }

    #[test]
    fn queue_order_and_history() {
        let mut p = Player::default();
        p.enqueue(vec![track("a"), track("b")], None);
        p.enqueue_next(vec![track("x"), track("y")], None);
        assert_eq!(titles(&p), ["x", "y", "a", "b"]);
        assert_eq!(p.advance().unwrap().track.title, "x");
        assert_eq!(p.advance().unwrap().track.title, "y");
        assert_eq!(p.previous().unwrap().track.title, "x");
        assert_eq!(titles(&p), ["y", "a", "b"]);
    }

    #[test]
    fn play_now_keeps_queue() {
        let mut p = Player::default();
        p.enqueue(vec![track("a")], None);
        p.play_now(vec![track("now"), track("then")], None);
        assert_eq!(p.current().unwrap().track.title, "now");
        assert_eq!(titles(&p), ["then", "a"]);
    }

    #[test]
    fn repeat_modes() {
        let mut p = Player::default();
        p.enqueue(vec![track("a"), track("b")], None);
        p.advance();
        p.repeat = RepeatMode::One;
        assert_eq!(p.advance().unwrap().track.title, "a");
        assert_eq!(p.skip().unwrap().track.title, "b", "skip ignores repeat-one");
        p.repeat = RepeatMode::All;
        assert_eq!(p.advance().unwrap().track.title, "b");
        assert_eq!(p.advance().unwrap().track.title, "b");
        p.repeat = RepeatMode::Off;
        p.advance();
        assert!(p.advance().is_none());
    }

    #[test]
    fn ids_are_unique_and_items_movable() {
        let mut p = Player::default();
        let items = p.enqueue(vec![track("a"), track("b"), track("c")], None);
        assert_eq!(items.iter().map(|i| i.id).collect::<Vec<_>>(), [1, 2, 3]);
        assert!(p.move_item(3, 0));
        assert_eq!(titles(&p), ["c", "a", "b"]);
        assert!(!p.move_item(99, 0));
        assert_eq!(p.remove(1).unwrap().track.title, "a");
        assert_eq!(p.jump(2).unwrap().track.title, "b");
        assert_eq!(p.current().unwrap().track.title, "b");
        assert_eq!(titles(&p), ["c"]);
        p.shuffle_queue();
        assert_eq!(p.queue().len(), 1);
    }
}
