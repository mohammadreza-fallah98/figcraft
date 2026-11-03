use crate::handoff::{FILE, Note, clear, now, read, write};

const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("figcraft-portal-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn notes_round_trip() {
    for note in [Note::Waiting { verifier: VERIFIER.into(), started: 1_700_000_000 }, Note::Done, Note::Failed("Code is invalid.".into())] {
        assert_eq!(Note::parse(&note.render()), Some(note));
    }
}

#[test]
fn junk_is_not_a_note() {
    for junk in [
        "",
        "waiting",
        "waiting\nshort\n1\n",
        format!("waiting\n{VERIFIER}\nsoon\n").as_str(),
        format!("waiting\n{VERIFIER}\n").as_str(),
        "hello\n",
        "\u{0}\u{0}",
    ] {
        assert_eq!(Note::parse(junk), None, "{junk:?}");
    }
}

#[test]
fn a_failure_message_is_one_short_line() {
    let rendered = Note::Failed(format!("a\nb\u{1b}{}", "x".repeat(500))).render();
    assert_eq!(rendered.lines().count(), 2);
    assert!(rendered.lines().nth(1).is_some_and(|m| m.chars().count() == 200 && m.starts_with("abx")));
}

#[test]
fn files_are_written_read_and_cleared() {
    let dir = temp("files");
    assert_eq!(read(&dir), None);
    let note = Note::Waiting { verifier: VERIFIER.into(), started: now() };
    write(&dir, &note).unwrap();
    assert!(dir.join(FILE).exists());
    assert_eq!(read(&dir), Some(note));
    write(&dir, &Note::Done).unwrap();
    assert_eq!(read(&dir), Some(Note::Done));
    clear(&dir);
    clear(&dir);
    assert_eq!(read(&dir), None);
    let _ = std::fs::remove_dir_all(&dir);
}
