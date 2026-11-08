//! `upper_invariant` must equal .NET 10 `ToUpperInvariant` for every code point (win-media §0.2): it feeds the video
//! cache identity, so a drift turns cached chunks into misses. The table was generated with .NET 10 on Windows 11.

use std::collections::HashMap;

#[test]
fn upper_invariant_matches_the_dotnet_table_for_every_code_point() {
    let table: HashMap<u32, u32> = include_str!("../data/upper-invariant.txt")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let (a, b) = l.split_once(':').unwrap();
            (u32::from_str_radix(a.trim(), 16).unwrap(), u32::from_str_radix(b.trim(), 16).unwrap())
        })
        .collect();
    assert!(table.len() > 1400);
    let mut mismatches = Vec::new();
    for cp in (0..=0x10FFFFu32).filter(|cp| !(0xD800..=0xDFFF).contains(cp)) {
        let c = char::from_u32(cp).unwrap();
        let expected = table.get(&cp).copied().unwrap_or(cp);
        let actual = sa_media::text::upper_invariant_char(c) as u32;
        if actual != expected {
            mismatches.push(format!("U+{cp:04X}: expected U+{expected:04X}, got U+{actual:04X}"));
        }
    }
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
    assert_eq!(sa_media::text::upper_invariant("ıißſé"), "ıIßSÉ");
}
