//! Speculative streaming for non-streaming engines (whisper family).
//!
//! The model can't stream, so we do the streaming around it: while
//! the user speaks, decode the unconfirmed audio in the background and
//! internally confirm stable segments (local agreement). On release
//! only the unconfirmed tail is decoded and merged. Nothing is ever
//! shown mid-hold; hypotheses stay internal until the final text.
//!
//! Rules that keep this honest:
//! - ONE inference at a time (single worker, next pass only after the
//!   previous lands). On slow machines passes space out by themselves
//!   instead of queueing stale audio behind a backlog.
//! - Agreement compares two hypotheses decoded from the SAME window
//!   start, word by word (case and punctuation ignored), minus a
//!   trailing unstable margin.
//! - Only whole segments are confirmed, and decoding resumes at the
//!   engine's own end timestamp of the last one: confirmed text and
//!   confirmed audio always describe the same speech.
//! - Confirmed text is appended, never replaced; the re-decoded
//!   overlap is dropped by timestamp, then by matching words.
//! - A failed or empty tail decode falls back to a full decode
//!   (accuracy over saved milliseconds).

/// Words at the hypothesis tail too unstable to confirm.
pub const UNSTABLE_TAIL_WORDS: usize = 3;
/// Cap on the overlap search when merging (words).
pub const MAX_OVERLAP_WORDS: usize = 12;

/// One decoded segment: its text and where it ends, in 16 kHz samples
/// from the start of the decoded window.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub text: String,
    pub end: usize,
}

/// Confirmed speculative output for one capture.
#[derive(Debug, Default, PartialEq)]
pub struct Agreement {
    /// Confirmed text, in speech order.
    pub text: String,
    /// Capture audio (16 kHz samples) that `text` covers.
    pub audio: usize,
    /// Previous hypothesis and the window start it was decoded from.
    prev: Option<(usize, Vec<Segment>)>,
}

fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

/// Comparison form of a word: "Hello," and "hello" agree.
fn norm(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn same(a: &[&str], b: &[&str]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| norm(x) == norm(y))
}

fn joined(segments: &[Segment]) -> String {
    segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

impl Agreement {
    /// Where the next decode window starts: confirmed audio minus the
    /// re-decoded overlap kept for context.
    pub fn window_start(&self, overlap: usize) -> usize {
        self.audio.saturating_sub(overlap)
    }

    /// Feed a hypothesis decoded from `len` samples at `start`.
    /// Confirms the leading whole segments both this and the previous
    /// hypothesis from the same start agree on, that end at least
    /// `unstable` samples before the window end.
    pub fn observe(&mut self, start: usize, len: usize, hyp: Vec<Segment>, unstable: usize) {
        let prev = match self.prev.replace((start, hyp)) {
            Some((prev_start, prev)) if prev_start == start => prev,
            // Different window base: texts are not comparable.
            _ => return,
        };
        let hyp = &self.prev.as_ref().expect("just stored").1;
        let (a, b) = (joined(&prev), joined(hyp));
        let (a, b) = (words(&a), words(&b));
        let agreed = a
            .iter()
            .zip(&b)
            .take_while(|(x, y)| norm(x) == norm(y))
            .count()
            .saturating_sub(UNSTABLE_TAIL_WORDS);
        let limit = len.saturating_sub(unstable);
        let (mut seen, mut take) = (0, 0);
        for (i, segment) in hyp.iter().enumerate() {
            seen += words(&segment.text).len();
            if seen > agreed || segment.end > limit {
                break;
            }
            take = i + 1;
        }
        let Some(last) = take.checked_sub(1).map(|i| &hyp[i]) else {
            return;
        };
        let end = start + last.end;
        if end <= self.audio {
            return;
        }
        let confirmed = hyp[..take].to_vec();
        self.text = self.append(start, &confirmed);
        self.audio = end;
        // The next window starts elsewhere: nothing left to compare with.
        self.prev = None;
    }

    /// Confirmed text followed by `hyp`, decoded from `start`. Segments
    /// ending inside the re-decoded overlap repeat confirmed speech.
    pub fn append(&self, start: usize, hyp: &[Segment]) -> String {
        let overlap = self.audio.saturating_sub(start);
        let fresh: Vec<Segment> = hyp.iter().filter(|s| s.end > overlap).cloned().collect();
        merge_tail(&self.text, &joined(&fresh))
    }
}

/// Merge committed text with a tail decode that overlaps it.
/// Finds the longest suffix-of-committed == prefix-of-tail match
/// (up to [`MAX_OVERLAP_WORDS`], case and punctuation ignored); plain
/// concatenation when nothing overlaps.
pub fn merge_tail(committed: &str, tail: &str) -> String {
    let a = words(committed);
    let b = words(tail);
    if a.is_empty() {
        return b.join(" ");
    }
    let max = MAX_OVERLAP_WORDS.min(a.len()).min(b.len());
    let overlap = (1..=max)
        .rev()
        .find(|&n| same(&a[a.len() - n..], &b[..n]))
        .unwrap_or(0);
    a.iter()
        .chain(&b[overlap..])
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_overlaps_or_concatenates() {
        assert_eq!(
            merge_tail("i want to create", "to create a new app"),
            "i want to create a new app"
        );
        assert_eq!(merge_tail("", "fresh start"), "fresh start");
        assert_eq!(merge_tail("done", ""), "done");
        // No overlap: plain concat.
        assert_eq!(merge_tail("hello world", "foo bar"), "hello world foo bar");
        // Punctuation and case do not hide an overlap.
        assert_eq!(
            merge_tail("Hello, world.", "world and more"),
            "Hello, world. and more"
        );
    }

    /// 0.5 s of constant samples per synthetic word.
    const BLOCK: usize = 8_000;
    /// 2 s: same margin as the live speculative loop.
    const UNSTABLE: usize = 4 * BLOCK;

    fn speech(words: usize) -> Vec<f32> {
        (1..=words)
            .flat_map(|w| std::iter::repeat_n(w as f32 / 1000.0, BLOCK))
            .collect()
    }

    /// Fake whisper over synthetic speech: one word per whole block,
    /// a segment every three words, punctuation that changes between
    /// calls, and a last word that is still wrong while speech arrives.
    fn decode(window: &[f32], call: usize, live: bool) -> Vec<Segment> {
        let blocks = window.len() / BLOCK;
        let (mut segments, mut text) = (Vec::new(), Vec::new());
        for b in 0..blocks {
            let id = (window[b * BLOCK] * 1000.0).round() as usize;
            let mut word = if live && b + 1 == blocks {
                format!("uh{call}")
            } else {
                format!("w{id}")
            };
            if call % 2 == 1 && b % 3 == 2 {
                word = format!("W{id},");
            }
            text.push(word);
            if text.len() == 3 || b + 1 == blocks {
                segments.push(Segment {
                    text: text.join(" "),
                    end: (b + 1) * BLOCK,
                });
                text.clear();
            }
        }
        segments
    }

    /// Ticks every `tick` words; speech goes on after the last tick.
    fn simulate(total: usize, tick: usize, overlap: usize) -> (String, Agreement) {
        let audio = speech(total);
        let mut agreement = Agreement::default();
        let (mut heard, mut call) = (tick, 0);
        while heard + 2 < total {
            let start = agreement.window_start(overlap);
            let window = &audio[start..heard * BLOCK];
            call += 1;
            agreement.observe(start, window.len(), decode(window, call, true), UNSTABLE);
            heard += tick;
        }
        let start = agreement.window_start(overlap);
        let tail = decode(&audio[start..], 0, false);
        (agreement.append(start, &tail), agreement)
    }

    #[test]
    fn rolling_windows_neither_drop_nor_repeat_speech() {
        for (total, tick, overlap) in [
            (60, 4, 0),
            (60, 4, 2 * BLOCK),
            (61, 3, 4 * BLOCK),
            (120, 5, BLOCK),
        ] {
            let (text, agreement) = simulate(total, tick, overlap);
            let heard: Vec<String> = text.split_whitespace().map(norm).collect();
            let spoken: Vec<String> = (1..=total).map(|w| format!("w{w}")).collect();
            assert_eq!(heard, spoken, "total={total} tick={tick} overlap={overlap}");
            // Confirmation actually shortened the release decode.
            assert!(
                agreement.audio > total * BLOCK / 2,
                "total={total} tick={tick} overlap={overlap}: confirmed only {}",
                agreement.audio
            );
        }
    }

    #[test]
    fn only_hypotheses_from_one_window_start_are_compared() {
        let mut agreement = Agreement::default();
        let seg = |text: &str, end| Segment {
            text: text.into(),
            end,
        };
        let hyp = || {
            vec![
                seg("a b c", 3 * BLOCK),
                seg("d e f", 6 * BLOCK),
                seg("g h i", 9 * BLOCK),
            ]
        };
        agreement.observe(0, 20 * BLOCK, hyp(), UNSTABLE);
        // Same words from another start describe different audio.
        agreement.observe(BLOCK, 20 * BLOCK, hyp(), UNSTABLE);
        assert_eq!((agreement.text.as_str(), agreement.audio), ("", 0));
        agreement.observe(BLOCK, 20 * BLOCK, hyp(), UNSTABLE);
        // Nine agreed words minus three unstable: two whole segments.
        assert_eq!(agreement.text, "a b c d e f");
        assert_eq!(agreement.audio, 7 * BLOCK);
    }
}
