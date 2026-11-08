//! .NET text helper tests: `upper_invariant` against the .NET 10 table (win-media §0.2), OrdinalIgnoreCase,
//! `SubtitleTime.Short` (PresentationTests.SubtitleTimeFormat) and `FormatByteSize`.

use sa_core::l10n::UiLanguage;
use sa_core::text::{
    cmp_ordinal_ignore_case, dotnet_fixed, eq_ordinal_ignore_case, format_byte_size_windows, guid_n, subtitle_time_short, upper_invariant,
    upper_invariant_char,
};

#[test]
fn upper_invariant_uses_simple_mapping_without_dotless_i() {
    assert_eq!(upper_invariant("ıißſé"), "ıIßSÉ");
    assert_eq!(upper_invariant("c:\\videos\\film.mp4"), "C:\\VIDEOS\\FILM.MP4");
    assert_eq!(upper_invariant("ǆ"), "Ǆ");
    assert_eq!(upper_invariant("ﬁ"), "ﬁ", "no full (multi-char) mapping");
    assert_eq!(upper_invariant("µ"), "Μ");
    assert_eq!(upper_invariant("ÿ"), "Ÿ");
    assert_eq!(upper_invariant("𐓘"), "𐒰", "supplementary planes are mapped");
}

#[test]
fn upper_invariant_matches_the_dotnet_table_for_every_code_point() {
    let table = include_str!("data/win-media-upper-invariant.txt");
    let mut mapped = std::collections::HashMap::new();
    for line in table.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let (from, to) = line.split_once(':').unwrap();
        mapped.insert(u32::from_str_radix(from, 16).unwrap(), u32::from_str_radix(to, 16).unwrap());
    }
    assert_eq!(mapped.len(), 1449);
    for cp in (0u32..=0x10FFFF).filter(|c| !(0xD800..=0xDFFF).contains(c)) {
        let c = char::from_u32(cp).unwrap();
        let expected = mapped.get(&cp).map(|&t| char::from_u32(t).unwrap()).unwrap_or(c);
        assert_eq!(upper_invariant_char(c), expected, "U+{cp:04X}");
    }
}

#[test]
fn ordinal_ignore_case_compares_utf16_units_of_the_uppercase_form() {
    assert!(eq_ordinal_ignore_case("Film.MP4", "film.mp4"));
    assert!(!eq_ordinal_ignore_case("ı", "I"));
    assert!(eq_ordinal_ignore_case("i", "I"));
    assert_eq!(cmp_ordinal_ignore_case("a", "B"), std::cmp::Ordering::Less);
    // UTF-16 order: U+FF21 (0xFF21) is after U+10400 (surrogates 0xD801 0xDC00), unlike scalar order.
    assert_eq!(cmp_ordinal_ignore_case("\u{FF21}", "\u{10400}"), std::cmp::Ordering::Greater);
}

#[test]
fn subtitle_time_format() {
    assert_eq!(subtitle_time_short(65.25), "01:05.25");
    assert_eq!(subtitle_time_short(0.0), "00:00.00");
    assert_eq!(subtitle_time_short(3600.5), "60:00.50");
    assert_eq!(subtitle_time_short(59.999), "00:60.00", "seconds round half away from zero like .NET");
    assert_eq!(subtitle_time_short(0.125), "00:00.13");
    assert_eq!(subtitle_time_short(6000.0), "100:00.00");
}

#[test]
fn format_byte_size_follows_the_ui_language() {
    assert_eq!(format_byte_size_windows(0, UiLanguage::Tr), "0 MB");
    assert_eq!(format_byte_size_windows(-5, UiLanguage::En), "0 MB");
    assert_eq!(format_byte_size_windows(512 * 1024, UiLanguage::Tr), "0,5 MB");
    assert_eq!(format_byte_size_windows(10 * 1024, UiLanguage::En), "0.01 MB");
    assert_eq!(format_byte_size_windows(1024 * 1024, UiLanguage::En), "1 MB");
    assert_eq!(format_byte_size_windows(1024 * 1024 * 3 / 2, UiLanguage::Tr), "1,5 MB");
    assert_eq!(format_byte_size_windows(1024 * 1024 * 1024, UiLanguage::Tr), "1 GB");
    assert_eq!(format_byte_size_windows(1024 * 1024 * 1024 * 5 / 4, UiLanguage::En), "1.25 GB");
    assert_eq!(format_byte_size_windows(20 * 1024 * 1024 * 1024, UiLanguage::En), "20 GB");
}

#[test]
fn dotnet_fixed_rounds_on_decimal_digits() {
    assert_eq!(dotnet_fixed(0.125, 2, 1), "0.13");
    assert_eq!(dotnet_fixed(2.675, 2, 1), "2.68");
    assert_eq!(dotnet_fixed(1.0, 0, 2), "01");
    assert_eq!(dotnet_fixed(-1.0, 0, 2), "-01");
    // .NET Core 3.0+ keeps the sign of a negative value that rounds to zero, and of -0.0 (C# fixture verified).
    assert_eq!(dotnet_fixed(-0.001, 2, 1), "-0.00");
    assert_eq!(dotnet_fixed(-0.0, 2, 2), "-00.00");
    assert_eq!(dotnet_fixed(0.006, 2, 1), "0.01");
    assert_eq!(dotnet_fixed(0.004, 0, 1), "0");
    assert_eq!(dotnet_fixed(9.995, 2, 1), "10.00");
    assert_eq!(dotnet_fixed(99.5, 0, 2), "100");
    assert_eq!(dotnet_fixed(1e20, 1, 1), "100000000000000000000.0");
    assert_eq!(dotnet_fixed(1.15, 1, 1), "1.2");
}

#[test]
fn guid_n_is_32_lowercase_hex() {
    let g = guid_n();
    assert_eq!(g.len(), 32);
    assert!(g.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    assert_ne!(g, guid_n());
}
