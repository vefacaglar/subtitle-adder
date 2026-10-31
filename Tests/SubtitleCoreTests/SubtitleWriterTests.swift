import Testing
@testable import SubtitleCore

private let sample = [
    Sentence(id: 1, start: 0, end: 1.5, text: "Merhaba.", words: []),
    Sentence(id: 2, start: 3661.007, end: 3662.5, text: "Görüşürüz.", words: []),
]

@Test func srtFormat() throws {
    let srt = try SubtitleWriter.render(sample, as: .srt)
    #expect(srt == """
    1
    00:00:00,000 --> 00:00:01,500
    Merhaba.

    2
    01:01:01,007 --> 01:01:02,500
    Görüşürüz.

    """)
}

@Test func vttFormat() throws {
    let vtt = try SubtitleWriter.render(sample, as: .vtt)
    #expect(vtt.hasPrefix("WEBVTT\n\n00:00:00.000 --> 00:00:01.500\nMerhaba.\n"))
}

@Test func jsonKeepsUnicode() throws {
    #expect(try SubtitleWriter.render(sample, as: .json).contains("Görüşürüz."))
}
