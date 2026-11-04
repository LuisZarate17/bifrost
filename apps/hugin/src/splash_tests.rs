use crate::splash::*;

fn seen(hwnd: isize, visible: bool, terminal_class: bool, pid: u32) -> Seen {
    Seen {
        hwnd,
        visible,
        terminal_class,
        pid,
    }
}

#[test]
fn the_layout_scales_with_the_display() {
    let one = layout(96);
    assert_eq!((one.card.right, one.card.bottom), (340, 104));
    assert_eq!((one.icon.width(), one.icon.bottom - one.icon.top), (48, 48));
    assert_eq!(one.track.bottom - one.track.top, 3);
    assert_eq!(one.track.width(), 340 - 2 * 18);
    let two = layout(192);
    assert_eq!((two.card.right, two.card.bottom), (680, 208));
    assert_eq!(two.icon.width(), 96);
    assert_eq!(two.border, 2);
}

#[test]
fn everything_sits_inside_the_card_and_the_text_is_right_of_the_shield() {
    for dpi in [96, 120, 144, 192, 288] {
        let l = layout(dpi);
        for rc in [l.icon, l.title, l.folder, l.track] {
            assert!(
                rc.left >= 0 && rc.top >= 0 && rc.right <= l.card.right && rc.bottom <= l.card.bottom,
                "{dpi}"
            );
        }
        assert!(l.title.left >= l.icon.right && l.folder.left >= l.icon.right);
        assert!(l.title.bottom <= l.folder.top + 1 && l.folder.bottom <= l.track.top);
    }
}

#[test]
fn the_sweep_crosses_the_track_eased_and_wraps() {
    let width = 300;
    // Off the left edge to begin with: nothing of it shows yet.
    assert_eq!(sweep(0, width), (0, 0));
    // Mid-sweep it is a third of the track wide and symmetrical about the middle.
    let (from, to) = sweep(550, width);
    assert_eq!(to - from, 100);
    // Eased: a tenth of the way in it has moved less than a tenth of the way.
    let (_, early) = sweep(110, width);
    assert!(early < 40, "{early}");
    // Past the right edge at the end, then the next sweep starts again from the left.
    assert_eq!(sweep(1099, width).1, width);
    assert_eq!(sweep(1100, width), sweep(0, width));
    // It never leaves the track.
    for t in (0..2300).step_by(7) {
        let (a, b) = sweep(t, width);
        assert!(0 <= a && a <= b && b <= width);
    }
}

#[test]
fn the_fade_in_rises_and_the_fade_out_falls_from_where_it_was() {
    assert_eq!(alpha(0, None), 0);
    assert_eq!(alpha(60, None), 127);
    assert_eq!(alpha(120, None), 255);
    assert_eq!(alpha(5000, None), 255);
    assert_eq!(alpha(0, Some((0, 255))), 255);
    assert_eq!(alpha(0, Some((80, 255))), 127);
    assert_eq!(alpha(0, Some((160, 255))), 0);
    assert_eq!(alpha(0, Some((400, 255))), 0);
    // Closed while still fading in at 100: it falls from there, not from full.
    assert_eq!(alpha(0, Some((0, 100))), 100);
}

#[test]
fn the_label_is_the_last_folder_unless_the_path_is_short() {
    assert_eq!(folder_label(r"C:\src\odin"), r"C:\src\odin");
    assert_eq!(folder_label(r"C:\Users\you\source\repos\bifrost"), "bifrost");
    assert_eq!(folder_label(r"C:\Users\you\source\repos\bifrost\"), "bifrost");
    assert_eq!(folder_label("."), ".");
}

#[test]
fn only_a_new_visible_terminal_window_counts() {
    let noted = [10, 11];
    // Nothing new yet: the old terminal windows, and a window that is not a terminal.
    let before = [
        seen(10, true, true, 1),
        seen(11, true, true, 2),
        seen(20, true, false, 3),
    ];
    assert!(!terminal_appeared(&noted, &before, 0));
    // A new terminal window that is not shown yet does not count; once shown, it does.
    let hidden = [seen(10, true, true, 1), seen(30, false, true, 4)];
    assert!(!terminal_appeared(&noted, &hidden, 0));
    let shown = [seen(10, true, true, 1), seen(30, true, true, 4)];
    assert!(terminal_appeared(&noted, &shown, 0));
    // A new window of another class is only the terminal when it is the process started.
    let other = [seen(40, true, false, 99)];
    assert!(!terminal_appeared(&noted, &other, 0));
    assert!(!terminal_appeared(&noted, &other, 7));
    assert!(terminal_appeared(&noted, &other, 99));
}
