//! Integration tests exercising realistic end-to-end workflows over the public API.

use metatrie::{Order, Record, Trie, Value};

/// Build a record from `(key, value)` pairs.
fn rec(pairs: &[(&str, Value)]) -> Record {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn field<'a>(r: &'a Record, key: &str) -> Option<&'a str> {
    match r.get(key) {
        Some(Value::String(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// Typeahead over city names: prefix suggestions ranked by population, plus a
/// typo-tolerant recovery.
#[test]
fn city_autocomplete_ranked_and_fuzzy() {
    let mut trie: Trie<Record> = Trie::new();
    let cities = [
        ("berlin", "Berlin", 3_600_000_i64),
        ("bern", "Bern", 130_000),
        ("bergen", "Bergen", 280_000),
        ("paris", "Paris", 2_100_000),
        ("solnechnogorsk", "Solnechnogorsk", 52_798),
    ];
    for (key, name, pop) in cities {
        trie.insert(key, rec(&[("name", name.into()), ("population", pop.into())]))
            .unwrap();
    }

    // Prefix "ber" → Berlin, Bergen, Bern ranked by population desc.
    let ranked = trie.get_by_prefix_sort_desc_by("ber", "population");
    let names: Vec<_> = ranked.iter().filter_map(|r| field(r, "name")).collect();
    assert_eq!(names, vec!["Berlin", "Bergen", "Bern"]);

    // Ascending flips the order.
    let asc = trie.get_by_prefix_sort_by("ber", "population", Order::Asc);
    let asc_names: Vec<_> = asc.iter().filter_map(|r| field(r, "name")).collect();
    assert_eq!(asc_names, vec!["Bern", "Bergen", "Berlin"]);

    // Exact prefix "solnecho" misses; one typo recovers Solnechnogorsk.
    assert!(trie.get_by_prefix("solnecho").is_empty());
    let fuzzy = trie.get_by_prefix_fuzzy("solnecho", 1).unwrap();
    assert_eq!(fuzzy.len(), 1);
    assert_eq!(field(fuzzy[0], "name"), Some("Solnechnogorsk"));
}

/// Issue-tracker style: autocomplete issue titles, then filter by metadata.
#[test]
fn issue_title_autocomplete_filtered_by_metadata() {
    let mut trie: Trie<Record> = Trie::new();
    let issues = [
        ("login fails on safari", "open", "high"),
        ("login redirect loop", "closed", "high"),
        ("logout button misaligned", "open", "low"),
        ("dashboard slow to load", "open", "medium"),
    ];
    for (title, status, priority) in issues {
        // Index by a normalized-ish key (lowercase, spaces kept).
        trie.insert(
            title,
            rec(&[
                ("title", title.into()),
                ("status", status.into()),
                ("priority", priority.into()),
            ]),
        )
        .unwrap();
    }

    // All titles starting with "log".
    let all_log = trie.get_by_prefix("log");
    assert_eq!(all_log.len(), 3);

    // Only open "log*" issues.
    let open = trie.get_by_prefix_and_query("log", &rec(&[("status", "open".into())]));
    let open_titles: Vec<_> = open.iter().filter_map(|r| field(r, "title")).collect();
    assert_eq!(
        open_titles,
        vec!["login fails on safari", "logout button misaligned"]
    );

    // Open AND high priority.
    let open_high = trie.get_by_prefix_and_query(
        "log",
        &rec(&[("status", "open".into()), ("priority", "high".into())]),
    );
    let oh: Vec<_> = open_high.iter().filter_map(|r| field(r, "title")).collect();
    assert_eq!(oh, vec!["login fails on safari"]);

    // Resolve one exact issue by title + a metadata constraint.
    let one = trie.get_by_word_and_query(
        "login redirect loop",
        &rec(&[("priority", "high".into())]),
    );
    assert_eq!(one.and_then(|r| field(r, "status")), Some("closed"));
}

/// Mutation workflow: insert, update, delete with pruning, count tracking.
#[test]
fn mutation_lifecycle() {
    let mut trie: Trie<Record> = Trie::new();
    trie.insert("alpha", rec(&[("v", 1.into())])).unwrap();
    trie.insert("alphabet", rec(&[("v", 2.into())])).unwrap();
    assert_eq!(trie.count(), 2);

    // Update replaces payloads.
    let old = trie.update("alpha", rec(&[("v", 99.into())])).unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(trie.count(), 2);

    // Delete "alphabet" prunes its tail but keeps "alpha".
    trie.delete("alphabet").unwrap();
    assert!(trie.has_prefix("alpha"));
    assert!(!trie.has_prefix("alphab"));
    assert_eq!(trie.count(), 1);

    // Delete the last word empties the trie entirely.
    trie.delete("alpha").unwrap();
    assert!(trie.is_empty());
    assert!(!trie.has_prefix("a"));
}

/// With a normalizer, differently-cased queries resolve the same entry.
#[cfg(feature = "normalize")]
#[test]
fn normalized_lookup_folds_case_and_diacritics() {
    let mut trie: Trie<Record> = Trie::with_normalizer(metatrie::normalize);
    trie.insert("Zürich", rec(&[("name", "Zürich".into())]))
        .unwrap();
    assert_eq!(trie.get_by_prefix("ZURI").len(), 1);
    assert_eq!(trie.get_by_prefix("zur").len(), 1);
}
