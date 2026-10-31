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

    /// Verilen `id`'ye sahip satırın indeksi.
    public func index(ofID id: Int) -> Int? {
        firstIndex { $0.id == id }
    }

    /// Başlangıcı ayarlar. Önceki satırın bitişinin (ya da 0'ın) altına inemez, kendi
    /// bitişine `minimumDuration`'dan fazla yaklaşamaz. Değişiklik olduysa `true`.
    @discardableResult
    public mutating func setStart(_ value: Double, at index: Int) -> Bool {
        guard indices.contains(index) else { return false }
        let lower = index > 0 ? self[index - 1].end : 0
        let upper = self[index].end - SentenceEditing.minimumDuration
        guard upper >= lower else { return false }
        let clamped = Swift.min(Swift.max(Self.rounded(value), lower), upper)
        guard clamped != self[index].start else { return false }
        self[index].start = clamped
        return true
    }

    /// Bitişi ayarlar. Sonraki satırın başlangıcını aşamaz, başlangıca `minimumDuration`'dan
    /// fazla yaklaşamaz. Değişiklik olduysa `true`.
    @discardableResult
    public mutating func setEnd(_ value: Double, at index: Int) -> Bool {
        guard indices.contains(index) else { return false }
        let lower = self[index].start + SentenceEditing.minimumDuration
        let upper = index + 1 < count ? self[index + 1].start : Double.infinity
        guard upper >= lower else { return false }
        let clamped = Swift.min(Swift.max(Self.rounded(value), lower), upper)
        guard clamped != self[index].end else { return false }
        self[index].end = clamped
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
    @discardableResult
    public mutating func mergeWithNext(at index: Int) -> Bool {
        guard indices.contains(index), indices.contains(index + 1) else { return false }
        let next = self[index + 1]
        let first = self[index]
        let text = [first.text, next.text]
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .joined(separator: " ")
        self[index] = Sentence(id: first.id, start: first.start, end: next.end, text: text, words: first.words + next.words)
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

    /// Kimlikleri 1'den başlayarak sıralı yapar.
    public mutating func renumber() {
        for index in indices { self[index].id = index + 1 }
    }

    private static func rounded(_ value: Double) -> Double {
        (value * 1000).rounded() / 1000
    }
}
