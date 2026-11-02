import Foundation

/// A shortcut at the top of the file sidebar.
public struct QuickLocation: Sendable, Hashable, Identifiable {
    public enum Kind: Sendable, Hashable {
        case movies
        case downloads
        case desktop
        /// A mounted disk other than the startup disk; the associated value is the volume's name.
        case volume(String)
    }

    public let kind: Kind
    public let url: URL

    public var id: String { MediaPath.normalized(url) }

    public init(kind: Kind, url: URL) {
        self.kind = kind
        self.url = url
    }
}

/// Where the sidebar starts: the user's Movies, Downloads and Desktop folders, then external disks, network shares and
/// disk images. Names are not provided; the app labels the kinds.
public enum QuickLocations {
    /// Movies, Downloads and Desktop that exist, then the visible mounted volumes except the startup disk, each path
    /// once.
    public static func current(fileManager: FileManager = .default) -> [QuickLocation] {
        var result: [QuickLocation] = []
        var seen: Set<String> = []
        func add(_ kind: QuickLocation.Kind, _ url: URL) {
            let path = MediaPath.normalized(url)
            if seen.insert(path.lowercased()).inserted { result.append(QuickLocation(kind: kind, url: url.standardizedFileURL)) }
        }

        let folders: [(QuickLocation.Kind, FileManager.SearchPathDirectory)] = [
            (.movies, .moviesDirectory), (.downloads, .downloadsDirectory), (.desktop, .desktopDirectory),
        ]
        for (kind, directory) in folders {
            guard let url = fileManager.urls(for: directory, in: .userDomainMask).first else { continue }
            var isDirectory: ObjCBool = false
            if fileManager.fileExists(atPath: url.path, isDirectory: &isDirectory), isDirectory.boolValue { add(kind, url) }
        }

        let keys: [URLResourceKey] = [.volumeNameKey, .volumeLocalizedNameKey]
        for url in fileManager.mountedVolumeURLs(includingResourceValuesForKeys: keys, options: [.skipHiddenVolumes]) ?? [] {
            guard MediaPath.normalized(url) != "/" else { continue }
            let values = try? url.resourceValues(forKeys: Set(keys))
            let name = values?.volumeName ?? values?.volumeLocalizedName ?? url.lastPathComponent
            add(.volume(name), url)
        }
        return result
    }
}
