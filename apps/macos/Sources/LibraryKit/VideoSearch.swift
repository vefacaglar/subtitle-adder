import Foundation

/// Case- and diacritic-insensitive comparison, independent of the user's locale ("daha sonra izle" equals "Daha Sonra İzle").
enum TextFolding {
    static func fold(_ text: String) -> String {
        text.folding(options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: nil)
    }
}

extension VideoUserStats {
    /// The library search. A blank query matches everything; otherwise the whole query must occur in the file name,
    /// folder or full path, or else every token of the query (split on space, tab, `_`, `-` and `.`) must, so
    /// "sezon 2 bolum" finds `Sezon_2_Bolum_05.mkv`. Case- and diacritic-insensitive.
    public func matches(search query: String?) -> Bool {
        let trimmed = (query ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return true }

        let haystacks = [fileName, directoryPath, videoPath].map(TextFolding.fold)
        let whole = TextFolding.fold(trimmed)
        if haystacks.contains(where: { $0.contains(whole) }) { return true }

        let separators: Set<Character> = [" ", "\t", "_", "-", "."]
        let tokens = trimmed.split(whereSeparator: { separators.contains($0) }).map { TextFolding.fold(String($0)) }
        guard !tokens.isEmpty else { return true }
        return tokens.allSatisfy { token in haystacks.contains { $0.contains(token) } }
    }
}
