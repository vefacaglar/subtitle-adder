import Foundation

/// `profiles.json`, version 2, in the same shape the Windows app writes: camelCase, nulls omitted, profiles sorted by
/// username. Lenient on read (unknown fields are ignored, missing ones default), so a file from either app opens in both.
struct ProfilesDocument: Codable {
    var version = 2
    var rememberMe = true
    var lastUsername: String?
    var rememberedUsername: String?
    var profiles: [ProfileDTO] = []

    init(rememberMe: Bool, lastUsername: String?, rememberedUsername: String?, profiles: [ProfileDTO]) {
        self.rememberMe = rememberMe
        self.lastUsername = lastUsername
        self.rememberedUsername = rememberedUsername
        self.profiles = profiles
    }

    init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decodeIfPresent(Int.self, forKey: .version) ?? 2
        rememberMe = try c.decodeIfPresent(Bool.self, forKey: .rememberMe) ?? true
        lastUsername = try c.decodeIfPresent(String.self, forKey: .lastUsername)
        rememberedUsername = try c.decodeIfPresent(String.self, forKey: .rememberedUsername)
        profiles = try c.decodeIfPresent([ProfileDTO].self, forKey: .profiles) ?? []
    }
}

struct ProfileDTO: Codable {
    var username = ""
    var saltBase64 = ""
    var passwordHashBase64 = ""
    var iterations = ProfileCrypto.defaultIterations
    var createdAtUtc = Date(timeIntervalSince1970: 0)
    var encryptedVaultBase64: String?

    init(username: String, saltBase64: String, passwordHashBase64: String, iterations: Int, createdAtUtc: Date, encryptedVaultBase64: String?) {
        self.username = username
        self.saltBase64 = saltBase64
        self.passwordHashBase64 = passwordHashBase64
        self.iterations = iterations
        self.createdAtUtc = createdAtUtc
        self.encryptedVaultBase64 = encryptedVaultBase64
    }

    init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        username = try c.decodeIfPresent(String.self, forKey: .username) ?? ""
        saltBase64 = try c.decodeIfPresent(String.self, forKey: .saltBase64) ?? ""
        passwordHashBase64 = try c.decodeIfPresent(String.self, forKey: .passwordHashBase64) ?? ""
        iterations = try c.decodeIfPresent(Int.self, forKey: .iterations) ?? ProfileCrypto.defaultIterations
        createdAtUtc = (try? c.decodeIfPresent(Date.self, forKey: .createdAtUtc)) ?? Date(timeIntervalSince1970: 0)
        encryptedVaultBase64 = try c.decodeIfPresent(String.self, forKey: .encryptedVaultBase64)
    }
}

/// The plaintext inside a profile's encrypted vault.
struct VaultPayload: Codable {
    var videos: [VideoUserStats] = []
    var customLists: [UserVideoList] = []
    /// Video key (fingerprint or path) to base64 image.
    var thumbnails: [String: String] = [:]

    init(videos: [VideoUserStats], customLists: [UserVideoList], thumbnails: [String: String]) {
        self.videos = videos
        self.customLists = customLists
        self.thumbnails = thumbnails
    }

    init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        videos = try c.decodeIfPresent([VideoUserStats].self, forKey: .videos) ?? []
        customLists = try c.decodeIfPresent([UserVideoList].self, forKey: .customLists) ?? []
        thumbnails = try c.decodeIfPresent([String: String].self, forKey: .thumbnails) ?? [:]
    }
}

enum ProfileJSON {
    static func encode<T: Encodable>(_ value: T, pretty: Bool) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = pretty ? [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes] : [.sortedKeys, .withoutEscapingSlashes]
        encoder.dateEncodingStrategy = .custom { date, encoder in
            var container = encoder.singleValueContainer()
            try container.encode(ProfileDateCoding.string(from: date))
        }
        return try encoder.encode(value)
    }

    static func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let text = try decoder.singleValueContainer().decode(String.self)
            guard let date = ProfileDateCoding.date(from: text) else {
                throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "Invalid date \(text)"))
            }
            return date
        }
        // Windows writes UTF-8 without a BOM, but its reader accepts one; accept it too.
        let bom: [UInt8] = [0xEF, 0xBB, 0xBF]
        let body = data.starts(with: bom) ? data.dropFirst(3) : data[...]
        return try decoder.decode(type, from: Data(body))
    }
}

/// ISO 8601 with 7 fractional digits and a numeric offset, the shape .NET's `DateTimeOffset` serializes to. Written by
/// hand (days-from-civil) so it is exact, locale-free and needs no shared formatter.
enum ProfileDateCoding {
    static func string(from date: Date) -> String {
        let total = date.timeIntervalSince1970
        var seconds = Int64(total.rounded(.down))
        var ticks = Int64(((total - Double(seconds)) * 10_000_000).rounded())
        if ticks >= 10_000_000 {
            seconds += 1
            ticks -= 10_000_000
        }
        let parts = fields(seconds)
        return String(
            format: "%04d-%02d-%02dT%02d:%02d:%02d.%07lld+00:00",
            parts.year, parts.month, parts.day, parts.hour, parts.minute, parts.second, ticks)
    }

    /// `yyyyMMdd-HHmmss` in UTC, for file names.
    static func compactTimestamp(_ date: Date) -> String {
        let parts = fields(Int64(date.timeIntervalSince1970.rounded(.down)))
        return String(format: "%04d%02d%02d-%02d%02d%02d", parts.year, parts.month, parts.day, parts.hour, parts.minute, parts.second)
    }

    /// Accepts `yyyy-MM-ddTHH:mm:ss[.f...][Z|+HH:mm|+HHmm|+HH]`; no offset means UTC.
    static func date(from text: String) -> Date? {
        let bytes = Array(text.utf8)
        var index = 0

        func number(_ digits: Int) -> Int? {
            guard index + digits <= bytes.count else { return nil }
            var value = 0
            for offset in 0..<digits {
                let byte = bytes[index + offset]
                guard byte >= 48, byte <= 57 else { return nil }
                value = value * 10 + Int(byte - 48)
            }
            index += digits
            return value
        }
        func expect(_ byte: UInt8) -> Bool {
            guard index < bytes.count, bytes[index] == byte else { return false }
            index += 1
            return true
        }

        guard let year = number(4), expect(UInt8(ascii: "-")), let month = number(2), expect(UInt8(ascii: "-")),
            let day = number(2), expect(UInt8(ascii: "T")), let hour = number(2), expect(UInt8(ascii: ":")),
            let minute = number(2), expect(UInt8(ascii: ":")), let second = number(2)
        else { return nil }
        guard (1...12).contains(month), (1...31).contains(day), hour < 24, minute < 60, second < 61 else { return nil }

        var fraction = 0.0
        if expect(UInt8(ascii: ".")) {
            var scale = 0.1
            var digits = 0
            while index < bytes.count, bytes[index] >= 48, bytes[index] <= 57 {
                fraction += Double(bytes[index] - 48) * scale
                scale /= 10
                index += 1
                digits += 1
            }
            guard digits > 0 else { return nil }
        }

        var offsetSeconds = 0
        if index < bytes.count {
            if bytes[index] == UInt8(ascii: "Z") || bytes[index] == UInt8(ascii: "z") {
                index += 1
            } else if bytes[index] == UInt8(ascii: "+") || bytes[index] == UInt8(ascii: "-") {
                let sign = bytes[index] == UInt8(ascii: "-") ? -1 : 1
                index += 1
                guard let offsetHours = number(2) else { return nil }
                _ = expect(UInt8(ascii: ":"))
                let offsetMinutes = number(2) ?? 0
                offsetSeconds = sign * (offsetHours * 3600 + offsetMinutes * 60)
            } else {
                return nil
            }
        }
        guard index == bytes.count else { return nil }

        let days = daysFromCivil(year: year, month: month, day: day)
        let seconds = days * 86_400 + Int64(hour * 3600 + minute * 60 + second - offsetSeconds)
        return Date(timeIntervalSince1970: Double(seconds) + fraction)
    }

    private static func fields(_ epochSeconds: Int64) -> (year: Int, month: Int, day: Int, hour: Int, minute: Int, second: Int) {
        var days = epochSeconds / 86_400
        var rest = epochSeconds % 86_400
        if rest < 0 {
            rest += 86_400
            days -= 1
        }
        let (year, month, day) = civilFromDays(days)
        return (year, month, day, Int(rest / 3600), Int(rest % 3600 / 60), Int(rest % 60))
    }

    // Howard Hinnant's days-from-civil / civil-from-days.
    private static func daysFromCivil(year: Int, month: Int, day: Int) -> Int64 {
        let y = Int64(month <= 2 ? year - 1 : year)
        let era = (y >= 0 ? y : y - 399) / 400
        let yearOfEra = y - era * 400
        let dayOfYear = (153 * Int64(month > 2 ? month - 3 : month + 9) + 2) / 5 + Int64(day) - 1
        let dayOfEra = yearOfEra * 365 + yearOfEra / 4 - yearOfEra / 100 + dayOfYear
        return era * 146_097 + dayOfEra - 719_468
    }

    private static func civilFromDays(_ days: Int64) -> (Int, Int, Int) {
        let z = days + 719_468
        let era = (z >= 0 ? z : z - 146_096) / 146_097
        let dayOfEra = z - era * 146_097
        let yearOfEra = (dayOfEra - dayOfEra / 1460 + dayOfEra / 36_524 - dayOfEra / 146_096) / 365
        let dayOfYear = dayOfEra - (365 * yearOfEra + yearOfEra / 4 - yearOfEra / 100)
        let mp = (5 * dayOfYear + 2) / 153
        let day = dayOfYear - (153 * mp + 2) / 5 + 1
        let month = mp < 10 ? mp + 3 : mp - 9
        let year = yearOfEra + era * 400 + (month <= 2 ? 1 : 0)
        return (Int(year), Int(month), Int(day))
    }
}
