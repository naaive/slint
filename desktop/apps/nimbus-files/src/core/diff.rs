// SPDX-License-Identifier: MIT

//! Edits that turn one ordered list into another, so a live refresh only touches the rows that changed.

use std::collections::HashSet;
use std::hash::Hash;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit<T> {
    Remove(usize),
    Insert(usize, T),
    Update(usize, T),
}

/// Plans edits from `old` to `new`, matching items by `key`; `None` when a full reset is cheaper.
///
/// Apply the edits in order: indices refer to the list as edited so far.
pub fn plan<T: Clone + PartialEq, K: Eq + Hash>(
    old: &[T],
    new: &[T],
    key: impl Fn(&T) -> K,
) -> Option<Vec<Edit<T>>> {
    let new_keys: HashSet<K> = new.iter().map(&key).collect();
    let mut edits = Vec::new();
    let mut current: Vec<&T> = Vec::with_capacity(old.len());
    for (i, item) in old.iter().enumerate().rev() {
        if !new_keys.contains(&key(item)) {
            edits.push(Edit::Remove(i));
        }
    }
    current.extend(old.iter().filter(|item| new_keys.contains(&key(item))));
    let budget = new.len().max(old.len()) / 2 + 8;
    for (i, item) in new.iter().enumerate() {
        if edits.len() > budget {
            return None;
        }
        match current.get(i) {
            Some(existing) if key(existing) == key(item) => {
                if *existing != item {
                    edits.push(Edit::Update(i, item.clone()));
                    current[i] = item;
                }
            }
            _ => {
                let item_key = key(item);
                if let Some(pos) = current.iter().skip(i).position(|c| key(c) == item_key) {
                    current.remove(i + pos);
                    edits.push(Edit::Remove(i + pos));
                }
                current.insert(i, item);
                edits.push(Edit::Insert(i, item.clone()));
            }
        }
    }
    (edits.len() <= budget).then_some(edits)
}

/// Applies edits to a vector, for tests and for models kept as plain vectors.
pub fn apply<T>(list: &mut Vec<T>, edits: Vec<Edit<T>>) {
    for edit in edits {
        match edit {
            Edit::Remove(i) => {
                list.remove(i);
            }
            Edit::Insert(i, item) => list.insert(i, item),
            Edit::Update(i, item) => list[i] = item,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(old: &[(&str, u32)], new: &[(&str, u32)]) -> Option<usize> {
        let edits = plan(old, new, |item| item.0)?;
        let count = edits.len();
        let mut list = old.to_vec();
        apply(&mut list, edits);
        assert_eq!(list, new);
        Some(count)
    }

    #[test]
    fn small_changes_are_incremental() {
        let old = [("a", 1), ("b", 1), ("c", 1), ("d", 1), ("e", 1)];
        assert_eq!(check(&old, &old), Some(0));
        assert_eq!(check(&old, &[("a", 1), ("b", 2), ("c", 1), ("d", 1), ("e", 1)]), Some(1));
        assert_eq!(check(&old, &[("a", 1), ("c", 1), ("d", 1), ("e", 1)]), Some(1));
        assert_eq!(
            check(&old, &[("a", 1), ("b", 1), ("bb", 1), ("c", 1), ("d", 1), ("e", 1)]),
            Some(1)
        );
        assert_eq!(check(&old, &[("e", 1), ("a", 1), ("b", 1), ("c", 1), ("d", 1)]), Some(2));
        assert_eq!(check(&[], &[("x", 1)]), Some(1));
        assert_eq!(check(&[("x", 1)], &[]), Some(1));
    }

    #[test]
    fn large_changes_reset() {
        let old: Vec<(&str, u32)> = [
            "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q",
            "r", "s", "t",
        ]
        .iter()
        .map(|k| (*k, 1))
        .collect();
        let mut reversed = old.clone();
        reversed.reverse();
        assert_eq!(check(&old, &reversed), None);
        let updated: Vec<(&str, u32)> = old.iter().map(|(k, _)| (*k, 2)).collect();
        assert_eq!(check(&old, &updated), None);
    }

    #[test]
    fn mixed_edits_produce_the_target() {
        let old = [("a", 1), ("b", 1), ("c", 1), ("d", 1), ("e", 1), ("f", 1), ("g", 1)];
        let new = [("b", 1), ("x", 1), ("d", 2), ("c", 1), ("g", 1), ("f", 1)];
        assert!(check(&old, &new).is_some());
    }
}
