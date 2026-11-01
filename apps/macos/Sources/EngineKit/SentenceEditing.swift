import Foundation

/// Constants used when editing subtitle lines.
public enum SentenceEditing {
    /// Shortest duration of a line (seconds).
    public static let minimumDuration: Double = 0.1
    /// Step size of the adjustment buttons (seconds).
    public static let nudgeStep: Double = 0.1
}

extension [Sentence] {
    /// Index of the line that should be shown at `time`: `start <= time < end`.
    public func activeIndex(at time: Double) -> Int? {
        firstIndex { $0.start <= time && time < $0.end }
    }

    /// Index of the line with the given stable identity (`uid`).
    public func index(ofUID uid: UUID) -> Int? {
        firstIndex { $0.uid == uid }
    }

    /// Sets the start. Cannot go below the previous line's end (or 0), and cannot come closer than
    /// `minimumDuration` to its own end. Returns `true` if something changed.
    ///
    /// If the value is already outside the valid range, only movement toward the range is allowed;
    /// it is never moved against the requested direction.
    @discardableResult
    public mutating func setStart(_ value: Double, at index: Int) -> Bool {
        guard let bounds = startBounds(at: index) else { return false }
        let current = self[index].start
        guard let target = clamped(value, from: current, to: bounds) else { return false }
        self[index].start = target
        return true
    }

    /// Sets the end. Cannot exceed the next line's start, and cannot come closer than `minimumDuration`
    /// to the start. Returns `true` if something changed. The direction rule is the same as `setStart`.
    @discardableResult
    public mutating func setEnd(_ value: Double, at index: Int) -> Bool {
        guard let bounds = endBounds(at: index) else { return false }
        let current = self[index].end
        guard let target = clamped(value, from: current, to: bounds) else { return false }
        self[index].end = target
        return true
    }

    @discardableResult
    public mutating func nudgeStart(at index: Int, by delta: Double) -> Bool {
        guard indices.contains(index) else { return false }
        return setStart(self[index].start + delta, at: index)
    }

    @discardableResult
    public mutating func nudgeEnd(at index: Int, by delta: Double) -> Bool {
        guard indices.contains(index) else { return false }
        return setEnd(self[index].end + delta, at: index)
    }

    @discardableResult
    public mutating func setText(_ text: String, at index: Int) -> Bool {
        guard indices.contains(index), self[index].text != text else { return false }
        self[index].text = text
        return true
    }

    /// Merges the line with the next: texts are joined with a space, the duration covers both.
    /// The first line's identity is kept.
    @discardableResult
    public mutating func mergeWithNext(at index: Int) -> Bool {
        guard indices.contains(index), indices.contains(index + 1) else { return false }
        let next = self[index + 1]
        let first = self[index]
        let text = [first.text, next.text]
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .joined(separator: " ")
        // Merging keeps the first line's speaker.
        self[index] = Sentence(
            id: first.id,
            uid: first.uid,
            start: first.start,
            end: next.end,
            text: text,
            words: first.words + next.words,
            speaker: first.speaker
        )
        remove(at: index + 1)
        renumber()
        return true
    }

    /// Assigns a speaker (zero-based) to the line. Returns `true` if something changed.
    @discardableResult
    public mutating func setSpeaker(_ speaker: Int?, at index: Int) -> Bool {
        guard indices.contains(index), self[index].speaker != speaker else { return false }
        self[index].speaker = speaker
        return true
    }

    /// Distinct non-nil speakers, sorted.
    public var speakers: [Int] {
        Swift.Array(Set(compactMap(\.speaker))).sorted()
    }

    /// An unused speaker index (one past the highest in use).
    public var nextNewSpeaker: Int { (speakers.last ?? -1) + 1 }

    /// Which lines get the "- " dialogue prefix; mirrors the engine's `speaker_dashes`: needs at least two
    /// distinct non-nil speakers, and a line gets one when it has a speaker that differs from the last
    /// known one (lines without a speaker do not reset it).
    public func dialogueDashes() -> [Bool] {
        guard speakers.count >= 2 else { return map { _ in false } }
        var previous: Int?
        return map { sentence in
            defer { if let speaker = sentence.speaker { previous = speaker } }
            return sentence.speaker != nil && sentence.speaker != previous
        }
    }

    /// Whether the line at `index` starts a speaker turn (gets the dash).
    public func startsDialogueTurn(at index: Int) -> Bool {
        guard indices.contains(index) else { return false }
        return dialogueDashes()[index]
    }

    /// Text as displayed: with the dialogue dash when the rule applies and the text has none yet.
    public func displayText(at index: Int) -> String {
        DialogueDash.apply(startsDialogueTurn(at: index), to: self[index].text)
    }

    @discardableResult
    public mutating func deleteSentence(at index: Int) -> Bool {
        guard indices.contains(index) else { return false }
        remove(at: index)
        renumber()
        return true
    }

    /// Splits the line in two at `time`. If word timestamps exist, the split point falls on the word
    /// boundary nearest the playback position; if there are no words, the text is split at the middle
    /// space and the duration is shared in proportion to the text length. The first
    /// part keeps the identity, the second gets a new one. If `time` is not inside the line,
    /// or the parts would be shorter than the minimum duration, nothing changes.
    @discardableResult
    public mutating func splitSentence(at index: Int, at time: Double) -> Bool {
        guard indices.contains(index) else { return false }
        let sentence = self[index]
        let playhead = Self.rounded(time)
        guard playhead > sentence.start, playhead < sentence.end,
              sentence.end - sentence.start >= 2 * SentenceEditing.minimumDuration
        else { return false }

        let lower = sentence.start + SentenceEditing.minimumDuration
        let upper = sentence.end - SentenceEditing.minimumDuration

        let firstWords: [Word]
        let secondWords: [Word]
        let firstText: String
        let secondText: String
        let rawBoundary: Double

        if sentence.words.count >= 2 {
            let leftCount = sentence.words.prefix { $0.midpoint < playhead }.count
            let splitIndex = Swift.min(Swift.max(leftCount, 1), sentence.words.count - 1)
            let left: [Word] = Swift.Array(sentence.words[0..<splitIndex])
            let right: [Word] = Swift.Array(sentence.words[splitIndex..<sentence.words.count])
            firstWords = left
            secondWords = right
            firstText = Self.joined(left)
            secondText = Self.joined(right)
            rawBoundary = (left[left.count - 1].end + right[0].start) / 2
        } else {
            let tokens = sentence.text.split(whereSeparator: \.isWhitespace).map(String.init)
            guard tokens.count >= 2 else { return false }
            let middle = tokens.count / 2
            let leftText = tokens[0..<middle].joined(separator: " ")
            let rightText = tokens[middle..<tokens.count].joined(separator: " ")
            firstText = leftText
            secondText = rightText
            let total = Double(leftText.count + rightText.count)
            rawBoundary = sentence.start
                + (sentence.end - sentence.start) * Double(leftText.count) / Swift.max(total, 1)
            firstWords = sentence.words.filter { $0.midpoint < Self.rounded(rawBoundary) }
            secondWords = sentence.words.filter { $0.midpoint >= Self.rounded(rawBoundary) }
        }

        guard !firstText.isEmpty, !secondText.isEmpty else { return false }
        let boundary = Self.rounded(Swift.min(Swift.max(rawBoundary, lower), upper))

        var first = sentence
        first.end = boundary
        first.text = firstText
        first.words = firstWords
        let second = Sentence(
            id: sentence.id + 1,
            start: boundary,
            end: sentence.end,
            text: secondText,
            words: secondWords,
            speaker: sentence.speaker
        )
        self[index] = first
        insert(second, at: index + 1)
        renumber()
        return true
    }

    /// Can the line be split at `time`? Uses the same conditions as `splitSentence` to decide
    /// whether the buttons are enabled.
    public func canSplit(at index: Int, at time: Double) -> Bool {
        guard indices.contains(index) else { return false }
        let sentence = self[index]
        let playhead = Self.rounded(time)
        guard playhead > sentence.start, playhead < sentence.end,
              sentence.end - sentence.start >= 2 * SentenceEditing.minimumDuration
        else { return false }
        if sentence.words.count >= 2 { return true }
        return sentence.text.split(whereSeparator: \.isWhitespace).count >= 2
    }

    /// Renumbers ids sequentially from 1; does not touch `uid` values.
    public mutating func renumber() {
        for index in indices { self[index].id = index + 1 }
    }

    private func startBounds(at index: Int) -> (lower: Double, upper: Double)? {
        guard indices.contains(index) else { return nil }
        let lower = index > 0 ? self[index - 1].end : 0
        let upper = self[index].end - SentenceEditing.minimumDuration
        guard upper >= lower else { return nil }
        return (lower, upper)
    }

    private func endBounds(at index: Int) -> (lower: Double, upper: Double)? {
        guard indices.contains(index) else { return nil }
        let lower = self[index].start + SentenceEditing.minimumDuration
        let upper = index + 1 < count ? self[index + 1].start : Double.infinity
        guard upper >= lower else { return nil }
        return (lower, upper)
    }

    /// Clamps the value to the valid range, but makes no change if the result would go against
    /// the requested direction. Values outside the range are moved only if the requested direction
    /// is toward validity.
    private func clamped(
        _ value: Double,
        from current: Double,
        to bounds: (lower: Double, upper: Double)
    ) -> Double? {
        let target = Swift.min(Swift.max(Self.rounded(value), bounds.lower), bounds.upper)
        guard target != current else { return nil }
        guard (value - current) * (target - current) > 0 else { return nil }
        return target
    }

    private static func joined(_ words: [Word]) -> String {
        words
            .map { $0.text.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .joined(separator: " ")
    }

    private static func rounded(_ value: Double) -> Double {
        (value * 1000).rounded() / 1000
    }
}

/// The dialogue dash applied to displayed text; same check as the engine's `_has_dash`.
public enum DialogueDash {
    /// Does the text already start with a dialogue dash ("- " or "\u{2013} ")?
    public static func hasDash(_ text: String) -> Bool {
        text.hasPrefix("- ") || text.hasPrefix("\u{2013} ")
    }

    public static func apply(_ dash: Bool, to text: String) -> String {
        dash && !hasDash(text) ? "- " + text : text
    }
}
