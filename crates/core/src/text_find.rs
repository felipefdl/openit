//! Forgiving text search shared by every reader: case, ligatures, accents, and
//! whitespace runs fold away before matching.

use std::ops::Range;

use unicode_normalization::UnicodeNormalization as _;
use unicode_normalization::char::is_combining_mark;

/// Fold `text` for matching.
///
/// NFKD splits ligatures, combining marks drop out, everything lowercases, and
/// whitespace runs collapse to one space. Every folded character carries the
/// index of the source character it came from.
#[must_use]
pub fn fold(text: &str) -> Vec<(char, usize)> {
  let mut out: Vec<(char, usize)> = Vec::with_capacity(text.len());
  for (index, source) in text.chars().enumerate() {
    if source.is_whitespace() {
      if out.last().is_none_or(|(last, _)| *last != ' ') {
        out.push((' ', index));
      }
      continue;
    }
    for decomposed in source.nfkd() {
      if is_combining_mark(decomposed) {
        continue;
      }
      let lowered = decomposed.to_lowercase().next().unwrap_or(decomposed);
      out.push((lowered, index));
    }
  }
  out
}

/// Every non-overlapping match of `query` in `text`, as byte ranges of `text`.
///
/// Matching folds as [`fold`] does. A match covers whole source characters,
/// so a query that ends inside a ligature still selects the ligature. A blank
/// query matches nothing.
#[must_use]
pub fn find(text: &str, query: &str) -> Vec<Range<usize>> {
  let needle: String = fold(query.trim()).into_iter().map(|(ch, _)| ch).collect();
  if needle.is_empty() {
    return Vec::new();
  }
  let needle_chars = needle.chars().count();
  let folded = fold(text);
  let haystack: String = folded.iter().map(|(ch, _)| *ch).collect();
  // Byte offset of every source character, plus the end, so a character
  // index maps to a byte range of `text` without rescanning.
  let offsets: Vec<usize> = text
    .char_indices()
    .map(|(byte, _)| byte)
    .chain(std::iter::once(text.len()))
    .collect();
  let mut ranges = Vec::new();
  let mut byte = 0_usize;
  let mut char_index = 0_usize;
  while let Some(rest) = haystack.get(byte..) {
    let Some(found) = rest.find(&needle) else {
      break;
    };
    let skipped = rest.get(..found).map_or(0, |skipped| skipped.chars().count());
    let start_char = char_index.saturating_add(skipped);
    let end_char = start_char.saturating_add(needle_chars);
    let window = folded.get(start_char..end_char).unwrap_or_default();
    let first = window.first().map(|(_, source)| *source);
    let last = window.last().map(|(_, source)| *source);
    if let (Some(first), Some(last)) = (first, last)
      && let (Some(&from), Some(&to)) = (offsets.get(first), offsets.get(last.saturating_add(1)))
    {
      ranges.push(from..to);
    }
    byte = byte.saturating_add(found).saturating_add(needle.len());
    char_index = end_char;
  }
  ranges
}

#[cfg(test)]
mod tests {
  use super::{find, fold};

  #[test]
  fn fold_splits_ligatures_drops_accents_and_lowercases() {
    let folded: String = fold("Confi\u{FB01}guração").iter().map(|(ch, _)| *ch).collect();
    assert_eq!(folded, "confifiguracao");

    let mapped: Vec<usize> = fold("\u{FB01}x").iter().map(|(_, index)| *index).collect();
    assert_eq!(mapped, vec![0, 0, 1], "both halves of the ligature point at one source char");
  }

  #[test]
  fn fold_collapses_whitespace() {
    let folded: String = fold("a \t\n b").iter().map(|(ch, _)| *ch).collect();
    assert_eq!(folded, "a b");
  }

  #[test]
  fn find_returns_source_byte_ranges_across_accents_and_case() {
    let text = "Ação e AÇÃO, depois acao.";

    let ranges = find(text, "acao");

    let found: Vec<&str> = ranges.iter().filter_map(|range| text.get(range.clone())).collect();
    assert_eq!(found, vec!["Ação", "AÇÃO", "acao"]);
  }

  #[test]
  fn find_matches_across_collapsed_whitespace_and_does_not_overlap() {
    let text = "one  two\none two";
    let found: Vec<&str> = find(text, "one two")
      .iter()
      .filter_map(|range| text.get(range.clone()))
      .collect();
    assert_eq!(found, vec!["one  two", "one two"]);

    assert_eq!(find("aaaa", "aa").len(), 2, "matches do not overlap");
  }

  #[test]
  fn find_widens_a_partial_ligature_to_the_whole_character() {
    let text = "e\u{FB01}x";
    let found: Vec<&str> = find(text, "ef").iter().filter_map(|range| text.get(range.clone())).collect();
    assert_eq!(found, vec!["e\u{FB01}"]);
  }

  #[test]
  fn find_ignores_a_blank_query() {
    assert!(find("anything", "   ").is_empty());
  }
}
