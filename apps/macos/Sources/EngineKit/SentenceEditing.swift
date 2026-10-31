import Foundation

/// Altyazı satırı düzenlemesinde kullanılan sabitler.
public enum SentenceEditing {
    /// Bir satırın en kısa süresi (saniye).
    public static let minimumDuration: Double = 0.1
    /// Ayar düğmelerinin adım büyüklüğü (saniye).
    public static let nudgeStep: Double = 0.1
}

extension [Sentence] {
    /// `time` anında gösterilmesi gereken satırın indeksi: `start <= time < end`.
    public func activeIndex(at time: Double) -> Int? {
        firstIndex { $0.start <= time && time < $0.end }
    }

    /// Verilen kararlı kimliğe (`uid`) sahip satırın indeksi.
    public func index(ofUID uid: UUID) -> Int? {
        firstIndex { $0.uid == uid }
    }

    /// Başlangıcı ayarlar. Önceki satırın bitişinin (ya da 0'ın) altına inemez, kendi
    /// bitişine `minimumDuration`'dan fazla yaklaşamaz. Değişiklik olduysa `true`.
    ///
    /// Değer zaten geçerli aralığın dışındaysa yalnızca aralığa doğru olan harekete izin
    /// verilir; istenen yönün tersine taşınmaz.
    @discardableResult
    public mutating func setStart(_ value: Double, at index: Int) -> Bool {
        guard let bounds = startBounds(at: index) else { return false }
        let current = self[index].start
        guard let target = clamped(value, from: current, to: bounds) else { return false }
        self[index].start = target
        return true
    }

    /// Bitişi ayarlar. Sonraki satırın başlangıcını aşamaz, başlangıca `minimumDuration`'dan
    /// fazla yaklaşamaz. Değişiklik olduysa `true`. Yön kuralı `setStart` ile aynıdır.
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

    /// Satırı sonrakiyle birleştirir: metinler boşlukla eklenir, süre ikisini kapsar.
    /// İlk satırın kimliği korunur.
    @discardableResult
    public mutating func mergeWithNext(at index: Int) -> Bool {
        guard indices.contains(index), indices.contains(index + 1) else { return false }
        let next = self[index + 1]
        let first = self[index]
        let text = [first.text, next.text]
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .joined(separator: " ")
        self[index] = Sentence(
            id: first.id,
            uid: first.uid,
            start: first.start,
            end: next.end,
            text: text,
            words: first.words + next.words
        )
        remove(at: index + 1)
        renumber()
        return true
    }

    @discardableResult
    public mutating func deleteSentence(at index: Int) -> Bool {
        guard indices.contains(index) else { return false }
        remove(at: index)
        renumber()
        return true
    }

    /// Satırı `time` anından ikiye böler. Kelime zaman damgaları varsa bölme noktası
    /// oynatma konumuna en yakın kelime sınırına denk gelir; kelime yoksa metin ortadaki
    /// boşluktan ikiye ayrılır ve süre metin uzunluğuna göre orantılı paylaştırılır. İlk
    /// parça kimliğini korur, ikinci parça yeni bir kimlik alır. `time` satırın içinde
    /// değilse ya da parçalar en kısa süreden kısa kalıyorsa değişiklik yapılmaz.
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
            words: secondWords
        )
        self[index] = first
        insert(second, at: index + 1)
        renumber()
        return true
    }

    /// `time` anında satır bölünebilir mi? Düğmelerin etkinliği için `splitSentence` ile
    /// aynı koşulları kullanır.
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

    /// Kimlikleri 1'den başlayarak sıralı yapar; `uid` değerlerine dokunmaz.
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

    /// Değeri geçerli aralığa sıkıştırır, ancak sonuç istenen yönün tersine düşüyorsa
    /// değişiklik yapmaz. Aralık dışındaki değerler yalnızca istenen yön geçerliliğe
    /// doğruysa taşınır.
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
