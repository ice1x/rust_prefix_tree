"""Black-box tests over the installed `metatrie` wheel (PyO3 bindings)."""

import pytest

from metatrie import Trie


def make_people():
    t = Trie()
    t.insert("иван", {"name": "иван", "age": 31, "gender": True, "type": True})
    t.insert("ирина", {"name": "ирина", "age": 23, "gender": False, "type": True})
    t.insert("ио", {"name": "ио", "age": 3, "gender": True, "type": False})
    t.insert("иванович", {"name": "иванович", "age": 51, "gender": True, "type": False})
    return t


def names(records):
    return [r["name"] for r in records]


def test_insert_and_count():
    t = make_people()
    assert t.count() == 4
    assert not t.is_empty()


def test_get_exact_and_missing():
    t = make_people()
    assert names(t.get("иван")) == ["иван"]
    assert t.get("нет") is None
    # Intermediate prefix is not a complete word.
    assert t.get("ива") is None


def test_has_prefix():
    t = make_people()
    assert t.has_prefix("ив")
    assert t.has_prefix("")  # whole tree
    assert not t.has_prefix("xyz")


def test_get_by_prefix_whole_tree():
    t = make_people()
    assert len(t.get_by_prefix("")) == 4
    assert t.get_by_prefix("zzz") == []


def test_sort_by_field_desc_and_asc():
    t = make_people()
    desc = t.get_by_prefix_sort_by("ив", "age")  # descending by default
    assert names(desc) == ["иванович", "иван"]
    asc = t.get_by_prefix_sort_by("и", "age", False)
    assert names(asc) == ["ио", "ирина", "иван", "иванович"]


def test_prefix_and_query():
    t = make_people()
    got = t.get_by_prefix_and_query("и", {"type": True, "gender": False})
    assert names(got) == ["ирина"]


def test_word_and_query():
    t = make_people()
    assert t.get_by_word_and_query("иван", {"gender": True})["name"] == "иван"
    assert t.get_by_word_and_query("иван", {"gender": False}) is None
    assert t.get_by_word_and_query("нет", {}) is None


def test_fuzzy_recovers_typo():
    t = Trie()
    t.insert("solnechnogorsk", {"name": "Solnechnogorsk"})
    assert t.get_by_prefix("solnecho") == []
    got = t.get_by_prefix_fuzzy("solnecho", 1)
    assert names(got) == ["Solnechnogorsk"]


def test_fuzzy_rejects_too_many_edits():
    t = Trie()
    t.insert("abc", {"v": 1})
    with pytest.raises(ValueError):
        t.get_by_prefix_fuzzy("abc", 99)


def test_update_and_delete():
    t = make_people()
    old = t.update("иван", {"name": "иван", "age": 100})
    assert old[0]["age"] == 31
    assert t.count() == 4

    removed = t.delete("ио")
    assert removed[0]["name"] == "ио"
    assert t.count() == 3
    with pytest.raises(KeyError):
        t.delete("нет")


def test_insert_empty_key_raises():
    t = Trie()
    with pytest.raises(ValueError):
        t.insert("", {"v": 1})


def test_clear():
    t = make_people()
    t.clear()
    assert t.is_empty()
    assert t.count() == 0
