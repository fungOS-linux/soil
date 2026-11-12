use spore_guest::{Command, Frame, Guest};

fn frame() -> Frame {
    Frame {
        width: 640,
        height: 360,
        pointer: (120.0, 100.0),
        hover: "nav.neo.neo".into(),
        pressed: String::new(),
        focus: "nav.neo.neo".into(),
        selected: "studio".into(),
        scroll: 0.0,
    }
}

#[test]
fn the_same_view_paints_the_same_scene() {
    let mut guest = Guest::new(640, 360).expect("guest starts");
    let (state, commands) = guest.present(&frame()).expect("frame renders");

    assert_eq!(state.width, 640);
    assert!(state.generation >= 1);

    let count = |kind: &str| {
        commands.iter().filter(|command| matches!(
            (kind, command),
            ("rect", Command::Rect { .. }) | ("text", Command::Text { .. })
                | ("hit", Command::Hit { .. }) | ("group", Command::Group(_))
        )).count()
    };

    // The native host and the browser produce these same numbers; a change
    // here means the view took a different path, not that the painter differs.
    assert_eq!(commands.len(), 34, "command count drifted from parity/native.trace");
    assert_eq!(count("rect"), 8);
    assert_eq!(count("text"), 9);
    assert_eq!(count("hit"), 5);
    assert_eq!(count("group"), 12);
}

#[test]
fn selection_is_the_hosts_to_change() {
    let mut guest = Guest::new(640, 360).unwrap();
    let mut frame = frame();
    frame.selected = "neo".into();
    let (_, commands) = guest.present(&frame).unwrap();
    // The view styles from what it is told; nothing in it remembers a click.
    assert!(commands.iter().any(|command| matches!(command, Command::Text { value, .. } if value == "neo")));
}
