//! Next-occurrence search behind Cmd/Ctrl+D: exact, case-sensitive matches
//! read straight from the editor's rope, without copying the document.

use std::ops::Range;

use ropey::Rope;

/// The word touching `offset`: the run of alphanumeric characters and `_` on
/// either side of it. `None` when neither neighbor is a word character.
#[must_use]
pub fn word_at(text: &Rope, offset: usize) -> Option<Range<usize>> {
  let offset = text.floor_char_boundary(offset.min(text.len()));
  let before: usize = text
    .chars_at(offset)
    .reversed()
    .take_while(|ch| is_word_char(*ch))
    .map(char::len_utf8)
    .sum();
  let after: usize = text
    .chars_at(offset)
    .take_while(|ch| is_word_char(*ch))
    .map(char::len_utf8)
    .sum();
  let range = offset - before..offset + after;
  (!range.is_empty()).then_some(range)
}

/// The first occurrence of `needle` that starts at or after `from`, wrapping
/// around to the start of `text`.
///
/// With `whole_word`, a match must not touch a word character on either side.
/// An empty needle matches nothing.
#[must_use]
pub fn next_occurrence(text: &Rope, needle: &str, from: usize, whole_word: bool) -> Option<Range<usize>> {
  let needle = needle.as_bytes();
  if needle.is_empty() {
    return None;
  }
  let from = text.floor_char_boundary(from.min(text.len()));
  let failure = failure_table(needle);
  let search = |start, start_limit| find_from(text, needle, &failure, start..start_limit, whole_word);
  search(from, text.len()).or_else(|| search(0, from))
}

/// The first match whose start lies in `starts`.
///
/// Knuth-Morris-Pratt over the rope's bytes. A byte match of a UTF-8 needle
/// always lands on character boundaries, so no decoding is needed.
fn find_from(
  text: &Rope,
  needle: &[u8],
  failure: &[usize],
  starts: Range<usize>,
  whole_word: bool,
) -> Option<Range<usize>> {
  let mut matched = 0_usize;
  for (end, byte) in (starts.start + 1..).zip(text.bytes_at(starts.start)) {
    if end >= starts.end + needle.len() {
      return None;
    }
    while matched > 0 && needle.get(matched) != Some(&byte) {
      matched = failure.get(matched - 1).copied().unwrap_or(0);
    }
    if needle.get(matched) == Some(&byte) {
      matched += 1;
    }
    if matched == needle.len() {
      let found = end - needle.len()..end;
      if !whole_word || is_whole_word(text, &found) {
        return Some(found);
      }
      matched = failure.get(matched - 1).copied().unwrap_or(0);
    }
  }
  None
}

/// For each needle prefix, the length of its longest proper prefix that is
/// also its suffix.
fn failure_table(needle: &[u8]) -> Vec<usize> {
  let mut table = vec![0_usize; needle.len()];
  let mut length = 0_usize;
  for (index, byte) in needle.iter().enumerate().skip(1) {
    while length > 0 && needle.get(length) != Some(byte) {
      length = table.get(length - 1).copied().unwrap_or(0);
    }
    if needle.get(length) == Some(byte) {
      length += 1;
    }
    if let Some(slot) = table.get_mut(index) {
      *slot = length;
    }
  }
  table
}

fn is_whole_word(text: &Rope, range: &Range<usize>) -> bool {
  let before = text.chars_at(range.start).reversed().next();
  let after = text.chars_at(range.end).next();
  !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
}

fn is_word_char(ch: char) -> bool {
  ch.is_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
  use ropey::Rope;

  use super::{next_occurrence, word_at};

  #[test]
  fn word_at_covers_the_word_on_either_side_of_the_caret() {
    let text = Rope::from_str("let café_au = 1;");

    assert_eq!(word_at(&text, 4), Some(4..12), "caret at the word start");
    assert_eq!(word_at(&text, 8), Some(4..12), "caret inside, after a multibyte char");
    assert_eq!(word_at(&text, 12), Some(4..12), "caret at the word end");
    assert_eq!(word_at(&text, 13), None, "caret between a space and punctuation");
  }

  #[test]
  fn next_occurrence_starts_at_from_and_wraps_to_the_start() {
    let text = Rope::from_str("foo bar foo baz foo");

    assert_eq!(next_occurrence(&text, "foo", 3, false), Some(8..11));
    assert_eq!(next_occurrence(&text, "foo", 11, false), Some(16..19));
    assert_eq!(next_occurrence(&text, "foo", 19, false), Some(0..3), "wraps past the end");
    assert_eq!(next_occurrence(&text, "qux", 0, false), None);
    assert_eq!(next_occurrence(&text, "", 0, false), None);
  }

  #[test]
  fn next_occurrence_is_case_sensitive_and_whole_word_skips_embedded_matches() {
    let text = Rope::from_str("id Id ids _id id");

    assert_eq!(
      next_occurrence(&text, "id", 2, false),
      Some(6..8),
      "Id differs in case; ids contains id"
    );
    assert_eq!(
      next_occurrence(&text, "id", 2, true),
      Some(14..16),
      "ids and _id are other words"
    );
  }

  #[test]
  fn next_occurrence_finds_a_match_that_spans_rope_chunks() {
    // Large enough that the rope splits into many chunks; every match lands
    // at a different chunk offset.
    let source = "abcdefgh needle ".repeat(4096);
    let text = Rope::from_str(&source);
    let mut from = 0;
    for _ in 0..4096 {
      let found = next_occurrence(&text, "needle", from, true).unwrap();
      assert_eq!(source.get(found.clone()), Some("needle"));
      from = found.end;
    }
    assert_eq!(
      next_occurrence(&text, "needle", from, true),
      Some(9..15),
      "wraps after the last one"
    );
  }

  #[test]
  fn next_occurrence_handles_self_overlapping_needles() {
    let text = Rope::from_str("aaab aab");

    assert_eq!(next_occurrence(&text, "aab", 0, false), Some(1..4));
    assert_eq!(next_occurrence(&text, "aab", 4, false), Some(5..8));
  }
}
