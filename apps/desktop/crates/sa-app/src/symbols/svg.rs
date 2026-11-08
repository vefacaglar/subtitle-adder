//! The bundled vector stand-ins for the SF Symbols the SwiftUI app uses (mac-ui §14.5, plus the names the Rust tree
//! adds: `chevron.up`/`chevron.down` pop-up arrows, `circle.fill` and `person.badge.plus` in the speaker menu).
//!
//! Drawn for this project (MIT, like the app) on a 24 × 24 grid to resemble each SF Symbol at regular weight: strokes
//! of 1.7 units with round caps and joins, `.fill` variants as solid shapes with the inner marks knocked out. Every
//! icon is black on transparent, so Slint's `colorize` tints it exactly like a template SF Symbol. Used on every
//! target except macOS, and on macOS when AppKit has no symbol of that name.
//!
//! Classes (see [`document`]): `f` solid fill without stroke, `fs` solid fill with the round stroke (soft corners on
//! play/speaker shapes), `b` a heavier stroke for the tiny pop-up chevrons.

/// `(SF Symbol name, SVG body)`, sorted by name (binary search).
pub const SYMBOLS: &[(&str, &str)] = &[
    ("arrow.clockwise", r#"<path d="M18.9 9.4A7.6 7.6 0 1 1 12 5.3"/><path d="M9.6 2.9 12.2 5.3 9.6 7.7"/>"#),
    ("arrow.down.circle", r#"<circle cx="12" cy="12" r="9"/><path d="M12 7.2v9.4M8.3 12.9 12 16.6l3.7-3.7"/>"#),
    (
        "arrow.down.right.and.arrow.up.left",
        r#"<path d="M4.5 10.5h6v-6M10.5 10.5 4 4M19.5 13.5h-6v6M13.5 13.5 20 20"/>"#,
    ),
    (
        "arrow.triangle.2.circlepath",
        r#"<path d="M5 10.6A7.2 7.2 0 0 1 18.4 8.6"/><path class="fs" d="M20.2 5.6 19.6 11 15 8.4z"/><path d="M19 13.4A7.2 7.2 0 0 1 5.6 15.4"/><path class="fs" d="M3.8 18.4 4.4 13 9 15.6z"/>"#,
    ),
    ("arrow.triangle.merge", r#"<path d="M6.5 20.5v-2.2c0-3.4 5.5-4.6 5.5-9V4.5M17.5 20.5v-2.2c0-3.4-5.5-4.6-5.5-9"/><path class="fs" d="M8.6 7.4 12 3.4l3.4 4z"/>"#),
    ("arrow.up", r#"<path d="M12 20V4.6M5.8 10.8 12 4.6l6.2 6.2"/>"#),
    (
        "arrow.up.left.and.arrow.down.right",
        r#"<path d="M4.5 10.5v-6h6M4.5 4.5 11 11M19.5 13.5v6h-6M19.5 19.5 13 13"/>"#,
    ),
    (
        "arrow.up.right.square",
        r#"<path d="M11 4H6.5A2.5 2.5 0 0 0 4 6.5v11A2.5 2.5 0 0 0 6.5 20h11a2.5 2.5 0 0 0 2.5-2.5V13"/><path d="M10.5 13.5 20 4M14 4h6v6"/>"#,
    ),
    (
        "captions.bubble",
        r#"<path d="M6 4h12a3 3 0 0 1 3 3v7.5a3 3 0 0 1-3 3h-7l-4.6 3.6v-3.6H6a3 3 0 0 1-3-3V7a3 3 0 0 1 3-3z"/><path d="M7 9.5h4M13.5 9.5H17M7 13h7M16.5 13h.5"/>"#,
    ),
    (
        "captions.bubble.fill",
        r##"<mask id="m"><rect width="24" height="24" fill="#fff" stroke="none"/><path d="M7 9.5h4M13.5 9.5H17M7 13h7M16.5 13h.5"/></mask><path class="fs" mask="url(#m)" d="M6 4h12a3 3 0 0 1 3 3v7.5a3 3 0 0 1-3 3h-7l-4.6 3.6v-3.6H6a3 3 0 0 1-3-3V7a3 3 0 0 1 3-3z"/>"##,
    ),
    (
        "character.bubble",
        r#"<path d="M12 3.5c5 0 9 3.3 9 7.5s-4 7.5-9 7.5c-.9 0-1.8-.1-2.6-.3L5 20.5l1-3.6C4.2 15.5 3 13.4 3 11c0-4.2 4-7.5 9-7.5z"/><path d="M9.2 14.4 12 7.4l2.8 7M10.1 12.3h3.8"/>"#,
    ),
    ("checkmark", r#"<path d="M4.8 12.8 9.6 17.5 19.2 6.5"/>"#),
    (
        "checkmark.circle.fill",
        r##"<mask id="m"><rect width="24" height="24" fill="#fff" stroke="none"/><path d="M7.6 12.3 10.7 15.3 16.4 8.8" stroke-width="2"/></mask><circle class="f" mask="url(#m)" cx="12" cy="12" r="9.5"/>"##,
    ),
    ("chevron.backward", r#"<path d="M15 4.5 7.5 12l7.5 7.5"/>"#),
    ("chevron.down", r#"<path class="b" d="M4.5 8.5 12 16l7.5-7.5"/>"#),
    ("chevron.right", r#"<path d="M9 4.5 16.5 12 9 19.5"/>"#),
    ("chevron.up", r#"<path class="b" d="M4.5 15.5 12 8l7.5 7.5"/>"#),
    ("circle", r#"<circle cx="12" cy="12" r="9"/>"#),
    ("circle.fill", r#"<circle class="f" cx="12" cy="12" r="9.5"/>"#),
    ("clock", r#"<circle cx="12" cy="12" r="9"/><path d="M12 6.8V12l3.4 2.2"/>"#),
    (
        "clock.arrow.circlepath",
        r#"<path d="M4.6 12a8 8 0 1 1 2.3 5.7"/><path class="fs" d="M1.9 9.6h5.4L4.6 13.4z"/><path d="M12.6 7.8v4.6l3 1.8"/>"#,
    ),
    (
        "cloud",
        r#"<path d="M7 18.5h10.5a3.9 3.9 0 0 0 .5-7.8 6 6 0 0 0-11.6-.9A4.4 4.4 0 0 0 7 18.5z"/>"#,
    ),
    (
        "doc.text",
        r#"<path d="M7 3h6.8L19 8.2V19a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z"/><path d="M13.5 3.2V8.5h5.3M8.5 12.5h7M8.5 16h7"/>"#,
    ),
    (
        "exclamationmark.triangle",
        r#"<path d="M10.3 4.3a2 2 0 0 1 3.4 0l7.5 13.2a2 2 0 0 1-1.7 3H4.5a2 2 0 0 1-1.7-3z"/><path d="M12 9.3v4.7"/><circle class="f" cx="12" cy="17.1" r="1.1"/>"#,
    ),
    (
        "exclamationmark.triangle.fill",
        r##"<mask id="m"><rect width="24" height="24" fill="#fff" stroke="none"/><path d="M12 9.3v4.7" stroke-width="2"/><circle cx="12" cy="17.1" r="1.2" fill="#000" stroke="none"/></mask><path class="fs" mask="url(#m)" d="M10.3 4.3a2 2 0 0 1 3.4 0l7.5 13.2a2 2 0 0 1-1.7 3H4.5a2 2 0 0 1-1.7-3z"/>"##,
    ),
    (
        "externaldrive",
        r#"<path d="M6.3 5.5h11.4a2 2 0 0 1 1.9 1.4L21 11.5v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-5l1.4-4.6a2 2 0 0 1 1.9-1.4z"/><path d="M3 11.5h18"/><circle class="f" cx="17.2" cy="15" r="1.1"/>"#,
    ),
    (
        "externaldrive.badge.xmark",
        r##"<mask id="m"><rect width="24" height="24" fill="#fff" stroke="none"/><circle cx="18" cy="17.5" r="6" fill="#000" stroke="none"/></mask><g mask="url(#m)"><path d="M5.3 4h9.4a2 2 0 0 1 1.9 1.4L18 9.5v4.5a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V9.5l1.4-4.1A2 2 0 0 1 5.3 4z"/><path d="M2 9.5h16"/></g><path d="M15.6 15.1l4.8 4.8M20.4 15.1l-4.8 4.8"/>"##,
    ),
    (
        "eye",
        r#"<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="3.2"/>"#,
    ),
    (
        "eye.fill",
        r##"<mask id="m"><rect width="24" height="24" fill="#fff" stroke="none"/><circle cx="12" cy="12" r="4.4" fill="#000" stroke="none"/><circle cx="12" cy="12" r="2.6" fill="#fff" stroke="none"/></mask><path class="fs" mask="url(#m)" d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/>"##,
    ),
    (
        "film",
        r#"<rect x="3.5" y="3.5" width="17" height="17" rx="2.5"/><path d="M7.8 3.5v17M16.2 3.5v17M3.5 8h4.3M3.5 12h4.3M3.5 16h4.3M16.2 8h4.3M16.2 12h4.3M16.2 16h4.3"/>"#,
    ),
    (
        "film.stack",
        r#"<rect x="3.5" y="8" width="17" height="13" rx="2.3"/><path d="M5.8 5.3h12.4M8 2.8h8M7.8 8v13M16.2 8v13M3.5 12.3h4.3M3.5 16.7h4.3M16.2 12.3h4.3M16.2 16.7h4.3"/>"#,
    ),
    (
        "folder",
        r#"<path d="M3 7a2 2 0 0 1 2-2h4.2l2 2.2H19a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><path d="M3 10h18"/>"#,
    ),
    ("folder.fill", r#"<path class="fs" d="M3 7a2 2 0 0 1 2-2h4.2l2 2.2H19a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>"#),
    (
        "gearshape",
        r#"<path d="M9.53 5.13 10.86 4.79 11.01 2.35H12.99L13.14 4.79 14.47 5.13 15.11 5.39 16.29 6.09 18.12 4.48 19.52 5.88 17.91 7.71 18.61 8.89 18.87 9.53 19.21 10.86 21.65 11.01V12.99L19.21 13.14 18.87 14.47 18.61 15.11 17.91 16.29 19.52 18.12 18.12 19.52 16.29 17.91 15.11 18.61 14.47 18.87 13.14 19.21 12.99 21.65H11.01L10.86 19.21 9.53 18.87 8.89 18.61 7.71 17.91 5.88 19.52 4.48 18.12 6.09 16.29 5.39 15.11 5.13 14.47 4.79 13.14 2.35 12.99V11.01L4.79 10.86 5.13 9.53 5.39 8.89 6.09 7.71 4.48 5.88 5.88 4.48 7.71 6.09 8.89 5.39z" stroke-width="1.5"/><circle cx="12" cy="12" r="3.2"/>"#,
    ),
    (
        "globe",
        r#"<circle cx="12" cy="12" r="9"/><ellipse cx="12" cy="12" rx="4" ry="9"/><path d="M3 12h18M4.4 7.5h15.2M4.4 16.5h15.2"/>"#,
    ),
    (
        "heart",
        r#"<path d="M12 20.2S3.4 15.3 3 9.3C2.8 6.5 4.8 4.3 7.5 4.3c1.9 0 3.5 1.1 4.5 2.8 1-1.7 2.6-2.8 4.5-2.8 2.7 0 4.7 2.2 4.5 5-.4 6-9 10.9-9 10.9z"/>"#,
    ),
    (
        "heart.fill",
        r#"<path class="fs" d="M12 20.2S3.4 15.3 3 9.3C2.8 6.5 4.8 4.3 7.5 4.3c1.9 0 3.5 1.1 4.5 2.8 1-1.7 2.6-2.8 4.5-2.8 2.7 0 4.7 2.2 4.5 5-.4 6-9 10.9-9 10.9z"/>"#,
    ),
    (
        "house",
        r#"<path d="M3.2 11.6 12 4l8.8 7.6"/><path d="M5.8 9.6v9a1.9 1.9 0 0 0 1.9 1.9h8.6a1.9 1.9 0 0 0 1.9-1.9v-9"/><path d="M10 20.5v-5.4a.8.8 0 0 1 .8-.8h2.4a.8.8 0 0 1 .8.8v5.4"/>"#,
    ),
    (
        "info.circle",
        r#"<circle cx="12" cy="12" r="9"/><path d="M10.6 10.8H12v6M10.3 16.8h3.4"/><circle class="f" cx="12" cy="7.7" r="1.1"/>"#,
    ),
    (
        "list.bullet",
        r#"<circle class="f" cx="4.8" cy="6.5" r="1.3"/><circle class="f" cx="4.8" cy="12" r="1.3"/><circle class="f" cx="4.8" cy="17.5" r="1.3"/><path d="M9 6.5h11M9 12h11M9 17.5h11"/>"#,
    ),
    ("lock", r#"<rect x="5" y="10.5" width="14" height="10" rx="2.2"/><path d="M8 10.5V8a4 4 0 0 1 8 0v2.5"/>"#),
    ("magnifyingglass", r#"<circle cx="10.3" cy="10.3" r="6.6"/><path d="M15.2 15.2 20.5 20.5"/>"#),
    (
        "menubar.dock.rectangle",
        r#"<rect x="3" y="4" width="18" height="16" rx="2.5"/><path d="M3 8h18"/><path d="M8 16.5h8" stroke-width="2.6"/>"#,
    ),
    ("minus", r#"<path d="M5 12h14"/>"#),
    (
        "network",
        r#"<circle cx="12" cy="5" r="2.2"/><circle cx="5" cy="18.5" r="2.2"/><circle cx="19" cy="18.5" r="2.2"/><circle class="f" cx="12" cy="13" r="1.5"/><path d="M12 7.2v4.3M10.7 14 6.6 17.1M13.3 14l4.1 3.1"/>"#,
    ),
    (
        "paintbrush",
        r#"<path d="M20.4 3.6c-1.2 2.9-5.6 7.8-8.6 10.3l-1.7-1.7c2.5-3 7.4-7.4 10.3-8.6z"/><path d="M9.9 13.3c-2.3-.1-3.9 1.4-4.2 3.5-.2 1.5-1 2.4-2.4 2.9 4.5 1.4 8.4-.3 8.4-4.6"/>"#,
    ),
    (
        "pause.fill",
        r#"<rect class="f" x="5.8" y="4.5" width="4.2" height="15" rx="1.2"/><rect class="f" x="14" y="4.5" width="4.2" height="15" rx="1.2"/>"#,
    ),
    ("pencil", r#"<path d="M15.8 4.6a2 2 0 0 1 2.8 0l.8.8a2 2 0 0 1 0 2.8L8.5 19.1 4 20l.9-4.5z"/><path d="M14 6.4 17.6 10"/>"#),
    (
        "person.2",
        r#"<circle cx="9" cy="8" r="3.3"/><path d="M2.8 19.5c0-3.3 2.8-5.6 6.2-5.6s6.2 2.3 6.2 5.6z"/><path d="M15.2 4.9a3 3 0 1 1 1 5.8M17.2 13.8c2.4.3 4.1 2.2 4.1 4.9h-3.4"/>"#,
    ),
    (
        "person.2.fill",
        r#"<circle class="f" cx="9" cy="8" r="3.6"/><path class="fs" d="M2.8 19.5c0-3.3 2.8-5.6 6.2-5.6s6.2 2.3 6.2 5.6z"/><circle class="f" cx="16.6" cy="7.9" r="2.9"/><path class="fs" d="M17.2 13.6c2.4.3 4.1 2.3 4.1 5h-4.6c-.1-1.9-.8-3.6-2-4.7.8-.3 1.6-.4 2.5-.3z"/>"#,
    ),
    (
        "person.badge.plus",
        r#"<circle cx="10" cy="8" r="3.6"/><path d="M3.5 20c0-3.6 2.9-6 6.5-6 1.5 0 2.8.4 3.9 1.1M3.5 20h9"/><path d="M18.5 13v7M15 16.5h7"/>"#,
    ),
    (
        "person.crop.circle",
        r#"<circle cx="12" cy="12" r="9"/><circle cx="12" cy="9.6" r="3.2"/><path d="M6.2 18.4c1.3-2.2 3.4-3.5 5.8-3.5s4.5 1.3 5.8 3.5"/>"#,
    ),
    (
        "person.crop.circle.badge.checkmark",
        r##"<mask id="m"><rect width="24" height="24" fill="#fff" stroke="none"/><circle cx="18" cy="18" r="6.4" fill="#000" stroke="none"/></mask><g mask="url(#m)"><circle cx="11" cy="11" r="8.5"/><circle cx="11" cy="8.9" r="3"/><path d="M5.5 17.3c1.2-2 3.2-3.3 5.5-3.3s4.3 1.3 5.5 3.3"/></g><mask id="c"><rect width="24" height="24" fill="#fff" stroke="none"/><path d="M15.6 18.1 17.3 19.8 20.4 16.3" stroke-width="1.6"/></mask><circle class="f" mask="url(#c)" cx="18" cy="18" r="4.8"/>"##,
    ),
    (
        "person.crop.circle.fill",
        r##"<mask id="m"><rect width="24" height="24" fill="#fff" stroke="none"/><circle cx="12" cy="9.6" r="3.5" fill="#000" stroke="none"/><path d="M5.6 18.7c1.4-2.5 3.7-4 6.4-4s5 1.5 6.4 4C16.8 20.2 14.5 21 12 21s-4.8-.8-6.4-2.3z" fill="#000" stroke="none"/></mask><circle class="f" mask="url(#m)" cx="12" cy="12" r="9.5"/>"##,
    ),
    ("play.fill", r#"<path class="fs" d="M7.5 4.8v14.4a.8.8 0 0 0 1.2.7l11-7.2a.8.8 0 0 0 0-1.4l-11-7.2a.8.8 0 0 0-1.2.7z"/>"#),
    (
        "play.rectangle.on.rectangle",
        r#"<path d="M7.5 3.5h11a2.5 2.5 0 0 1 2.5 2.5v9"/><rect x="3" y="7.5" width="14.5" height="13" rx="2.3"/><path class="fs" d="M8.6 11v6l4.8-3z" stroke-width="1"/>"#,
    ),
    ("plus", r#"<path d="M12 4.5v15M4.5 12h15"/>"#),
    (
        "questionmark.folder",
        r#"<path d="M3 7a2 2 0 0 1 2-2h4.2l2 2.2H19a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><path d="M10.1 11.4a1.9 1.9 0 1 1 2.6 1.8c-.5.2-.7.6-.7 1.1v.3"/><circle class="f" cx="12" cy="16.6" r=".95"/>"#,
    ),
    (
        "rectangle.center.inset.filled",
        r#"<rect x="3" y="5" width="18" height="14" rx="2.5"/><rect class="f" x="7" y="8.5" width="10" height="7" rx="1"/>"#,
    ),
    (
        "rectangle.portrait.and.arrow.right",
        r#"<path d="M13 7.5V6a2.5 2.5 0 0 0-2.5-2.5h-4A2.5 2.5 0 0 0 4 6v12a2.5 2.5 0 0 0 2.5 2.5h4A2.5 2.5 0 0 0 13 18v-1.5"/><path d="M9 12h11.5M17.3 8.8l3.2 3.2-3.2 3.2"/>"#,
    ),
    (
        "rectangle.righthalf.inset.filled",
        r#"<rect x="3" y="5" width="18" height="14" rx="2.5"/><rect class="f" x="12.3" y="7.8" width="6" height="8.4" rx="1"/>"#,
    ),
    (
        "rectangle.stack.badge.plus",
        r#"<rect x="2.5" y="8.5" width="12.5" height="11" rx="2"/><path d="M4.8 5.7h8"/><path d="M19 12.5v7M15.5 16h7"/>"#,
    ),
    (
        "scissors",
        r#"<circle cx="6" cy="6.5" r="2.7"/><circle cx="6" cy="17.5" r="2.7"/><path d="M8.2 8.2 20 18M8.2 15.8 20 6"/>"#,
    ),
    (
        "scope",
        r#"<circle cx="12" cy="12" r="7.5"/><path d="M12 2.5v4M12 17.5v4M2.5 12h4M17.5 12h4"/><circle class="f" cx="12" cy="12" r="1.2"/>"#,
    ),
    (
        "sidebar.left",
        r#"<rect x="3" y="4.5" width="18" height="15" rx="2.5"/><path d="M9.5 4.5v15M5.6 8h1.6M5.6 11h1.6M5.6 14h1.6"/>"#,
    ),
    (
        "speaker.slash.fill",
        r##"<mask id="m"><rect width="24" height="24" fill="#fff" stroke="none"/><path d="M3.5 3.5 20.5 20.5" stroke-width="4"/></mask><path class="fs" mask="url(#m)" d="M3.5 9.5h3.6l5-4.2v13.4l-5-4.2H3.5z"/><path d="M15 8.6a4 4 0 0 1 .9 4.5" mask="url(#m)"/><path d="M3.8 3.8 20.2 20.2"/>"##,
    ),
    (
        "speaker.wave.2.fill",
        r#"<path class="fs" d="M3.5 9.5h3.6l5-4.2v13.4l-5-4.2H3.5z"/><path d="M15.2 9.1a4.2 4.2 0 0 1 0 5.8M17.9 6.4a8 8 0 0 1 0 11.2"/>"#,
    ),
    (
        "square.and.arrow.down",
        r#"<path d="M8.2 9H6.5A2 2 0 0 0 4.5 11v7.5a2 2 0 0 0 2 2h11a2 2 0 0 0 2-2V11a2 2 0 0 0-2-2h-1.7"/><path d="M12 3v11.5M8.6 11.2 12 14.6l3.4-3.4"/>"#,
    ),
    (
        "square.stack.3d.up",
        r#"<path d="M12 3 20.5 7.5 12 12 3.5 7.5z"/><path d="M3.5 12 12 16.5 20.5 12M3.5 16.5 12 21l8.5-4.5"/>"#,
    ),
    ("star", r#"<path d="M12 2.8 14.47 9 21.13 9.43 15.99 13.7 17.64 20.17 12 16.6 6.36 20.17 8.01 13.7 2.87 9.43 9.53 9z"/>"#),
    (
        "star.fill",
        r#"<path class="fs" d="M12 2.8 14.47 9 21.13 9.43 15.99 13.7 17.64 20.17 12 16.6 6.36 20.17 8.01 13.7 2.87 9.43 9.53 9z"/>"#,
    ),
    ("stop.fill", r#"<rect class="f" x="5" y="5" width="14" height="14" rx="2.4"/>"#),
    ("text.badge.plus", r#"<path d="M3.5 6h14M3.5 11h14M3.5 16h7.5M18 13v7.5M14.2 16.7h7.5"/>"#),
    ("text.badge.xmark", r#"<path d="M3.5 6h14M3.5 11h14M3.5 16h7.5M14.6 14l5.4 5.4M20 14l-5.4 5.4"/>"#),
    (
        "translate",
        r#"<path d="M2.8 5.2h9M7.3 3v2.2M4.6 5.2c.8 3.1 3.1 5.6 6.4 6.9M10 5.2c-.8 3.1-3.1 5.6-6.4 6.9M12.6 21l4.2-10.4L21 21M14.1 17.4h5.4"/>"#,
    ),
    (
        "trash",
        r#"<path d="M3.8 6.5h16.4M9.3 6.5V4.8a1 1 0 0 1 1-1h3.4a1 1 0 0 1 1 1v1.7M6 6.5l.9 12.6a2 2 0 0 0 2 1.9h6.2a2 2 0 0 0 2-1.9L18 6.5M10 10.5v6.5M14 10.5v6.5"/>"#,
    ),
    (
        "wand.and.stars",
        r#"<path d="M3.8 20.2 14.6 9.4"/><path d="M13.3 8.1l2.6 2.6" stroke-width="2.6"/><path class="fs" d="M8.5 2.4 9.2 4.3 11.1 5 9.2 5.7 8.5 7.6 7.8 5.7 5.9 5 7.8 4.3z" stroke-width="0.8"/><path class="fs" d="M18.5 2 19.4 4.6 22 5.5 19.4 6.4 18.5 9 17.6 6.4 15 5.5 17.6 4.6z" stroke-width="0.8"/><path class="fs" d="M19 12.8 19.6 14.4 21.2 15 19.6 15.6 19 17.2 18.4 15.6 16.8 15 18.4 14.4z" stroke-width="0.8"/>"#,
    ),
    ("waveform", r#"<path d="M3 10.8v2.4M6.3 8v8M9.6 4.5v15M12.9 8.5v7M16.2 6v12M19.5 9.5v5M22 11.3v1.4"/>"#),
    ("xmark", r#"<path d="M6 6l12 12M18 6 6 18"/>"#),
    (
        "xmark.circle.fill",
        r##"<mask id="m"><rect width="24" height="24" fill="#fff" stroke="none"/><path d="M8.6 8.6l6.8 6.8M15.4 8.6l-6.8 6.8" stroke-width="2"/></mask><circle class="f" mask="url(#m)" cx="12" cy="12" r="9.5"/>"##,
    ),
];

/// The SVG body of `name`, if the set has it.
pub fn body(name: &str) -> Option<&'static str> {
    SYMBOLS.binary_search_by(|(n, _)| (*n).cmp(name)).ok().map(|i| SYMBOLS[i].1)
}

/// A complete SVG document for `name` (24 × 24 user units, rendered by Slint at whatever size it is shown).
pub fn document(name: &str) -> Option<String> {
    document_sized(name, 24)
}

/// A complete SVG document for `name` whose intrinsic size is `px × px` (for rasterizing at an exact pixel size).
pub fn document_sized(name: &str, px: u32) -> Option<String> {
    body(name).map(|b| {
        format!(
            concat!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="{px}" height="{px}" viewBox="0 0 24 24" fill="none" "##,
                r##"stroke="#000" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><style>"##,
                r##".f{{fill:#000;stroke:none}}.fs{{fill:#000}}.b{{stroke-width:2.8}}</style>{body}</svg>"##
            ),
            px = px.max(1),
            body = b
        )
    })
}
