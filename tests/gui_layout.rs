//! The hardware-style windows' layouts: every panel key and encoder is on the
//! window, each place is hit at its centre, and no two places overlap (but the
//! piano keys, black over white). One test, in its own process: the panel
//! tables in use are a global (`panel::use_*_panel`).

use dtemu::gui::{self, Control, Place};
use dtemu::panel;

fn check(name: &str, places: &[Place], keys: &[(&str, u8, u8, u8)], encoders: usize) {
    let mut codes: Vec<u8> = Vec::new();
    let mut encs: Vec<u8> = Vec::new();
    for p in places {
        match p.c {
            Control::Key(c) => codes.push(c),
            Control::Knob(e, push) => {
                encs.push(e);
                codes.extend(push);
            }
        }
    }
    let mut sorted = codes.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), codes.len(), "{name}: a key placed twice");
    for k in keys {
        let c = panel::rytm_code(k.0);
        assert!(codes.contains(&c), "{name}: {} not on the window", k.0);
    }
    assert_eq!(codes.len(), keys.len(), "{name}: keys");
    assert_eq!(encs.len(), encoders, "{name}: encoders");
    let kb = |p: &Place| matches!(p.c, Control::Key(c) if panel::key_name(c).is_some_and(|n| n.starts_with("KB_")));
    for (i, p) in places.iter().enumerate() {
        let (cx, cy) = (p.x + p.w / 2, p.y + p.h / 2);
        let first = places.iter().position(|q| q.hit(cx, cy)).unwrap();
        assert_eq!(first, i, "{name}: place {i} is not hit at its centre");
        for q in &places[i + 1..] {
            let overlap = p.x < q.x + q.w && q.x < p.x + p.w && p.y < q.y + q.h && q.y < p.y + p.h;
            assert!(!overlap || (kb(p) && kb(q)), "{name}: places overlap at ({}, {}) and ({}, {})", p.x, p.y, q.x, q.y);
        }
    }
}

/// Draws without panicking and fits the window.
fn render(rytm: bool, a4: bool, mk1: bool, places: &[Place]) {
    let (w, h) = gui::window_size(rytm, a4, mk1);
    assert!(w <= 1400 && h <= 900);
    for p in places {
        assert!(p.x >= 0 && p.y >= 0 && p.x + p.w <= w as i32 && p.y + p.h <= h as i32, "place off the window");
    }
    let mut cv = gui::Canvas::new(w, h);
    let view = gui::View { rytm, a4, mk1, oled: vec![0x55; 1024], leds: vec![Some((255, 40, 30)); panel::ALL_LEDS], status: "TEST".into() };
    let held: Vec<u8> = places.iter().take(3).filter_map(|p| if let Control::Key(c) = p.c { Some(c) } else { None }).collect();
    gui::draw_sel(&mut cv, &view, places, &held, &held[..1], Some(0));
}

#[test]
fn hardware_layouts() {
    // The Rytm MKII's tables are the default.
    let places = gui::layout_for(true, false, false);
    check("Rytm MKII", &places, &panel::RYTM_KEYS, 9);
    render(true, false, false, &places);

    // The Digitakt: the tables are the default too, and its keys are the upstream
    // control codes 1..47; every one is on the window, by name.
    let places = gui::layout_for(false, false, false);
    let mut codes: Vec<u8> = places
        .iter()
        .filter_map(|p| match p.c {
            Control::Key(c) => Some(c),
            Control::Knob(_, push) => push,
        })
        .collect();
    codes.sort();
    assert_eq!(codes, (1..48).collect::<Vec<u8>>(), "Digitakt: keys");
    assert!(codes.iter().all(|&c| panel::key_name(c).is_some()), "Digitakt: a key without a name");
    assert_eq!(places.iter().filter(|p| matches!(p.c, Control::Knob(..))).count(), 9, "Digitakt: encoders");
    for (i, p) in places.iter().enumerate() {
        assert_eq!(places.iter().position(|q| q.hit(p.x + p.w / 2, p.y + p.h / 2)), Some(i), "Digitakt: place {i}");
        for q in &places[i + 1..] {
            assert!(!(p.x < q.x + q.w && q.x < p.x + p.w && p.y < q.y + q.h && q.y < p.y + p.h), "Digitakt: places overlap at ({}, {})", p.x, p.y);
        }
    }
    render(false, false, false, &places);

    panel::use_a4_panel();
    let places = gui::layout_for(false, true, false);
    check("A4 MKII", &places, &panel::A4_KEYS, 11);
    render(false, true, false, &places);

    panel::use_a4mk1_panel();
    let places = gui::layout_for(false, false, true);
    check("A4 MKI", &places, &panel::A4MK1_KEYS, 11);
    render(false, false, true, &places);

    panel::use_rytmmk1_panel();
    let places = gui::layout_for(false, false, true);
    check("Rytm MKI", &places, &panel::RYTMMK1_KEYS, 9);
    render(false, false, true, &places);

    panel::use_syntakt_panel();
    let places = gui::layout_for(false, false, false);
    check("Syntakt", &places, &panel::SYNTAKT_KEYS, 9);
    render(false, false, false, &places);
}
