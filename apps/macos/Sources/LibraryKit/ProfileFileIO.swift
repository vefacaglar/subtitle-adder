import Foundation

/// What `profiles.json` held when it was last read.
enum ProfileFileSnapshot: Equatable, Sendable {
    case missing
    /// Exists but could not be read (permissions, I/O error).
    case unreadable
    case content(Data)
}

/// Cheap change detector: `stat` fields that change whenever the file is replaced or rewritten.
struct FileSignature: Equatable, Sendable {
    let size: Int64
    let seconds: Int
    let nanoseconds: Int
    let inode: UInt64
}

/// The POSIX side of `profiles.json`: reading, atomic replacement and keeping a copy of a damaged file.
enum ProfileFileIO {
    static func signature(of url: URL) -> FileSignature? {
        var info = stat()
        guard stat(url.path, &info) == 0 else { return nil }
        return FileSignature(
            size: Int64(info.st_size), seconds: Int(info.st_mtimespec.tv_sec), nanoseconds: Int(info.st_mtimespec.tv_nsec),
            inode: UInt64(info.st_ino))
    }

    static func snapshot(of url: URL) -> ProfileFileSnapshot {
        do {
            return .content(try Data(contentsOf: url))
        } catch {
            return signature(of: url) == nil ? .missing : .unreadable
        }
    }

    /// Writes `data` to a private temp file next to `url` (mode 0600, fsynced) and renames it over `url`, so readers
    /// see either the old or the new file, never half of one.
    static func writeAtomically(_ data: Data, to url: URL) throws(ProfileError) {
        let temp = url.deletingLastPathComponent().appending(path: ".\(url.lastPathComponent).tmp-\(UUID().uuidString)")
        let fd = open(temp.path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0o600)
        guard fd >= 0 else { throw .writeFailed(errno: errno) }

        var failure: Int32 = 0
        data.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                let written = write(fd, raw.baseAddress! + offset, raw.count - offset)
                if written < 0 {
                    if errno == EINTR { continue }
                    failure = errno
                    return
                }
                offset += written
            }
        }
        if failure == 0, fsync(fd) != 0 { failure = errno }
        close(fd)
        if failure == 0, rename(temp.path, url.path) != 0 { failure = errno }
        if failure != 0 {
            unlink(temp.path)
            throw .writeFailed(errno: failure)
        }
    }

    /// Keeps the current (damaged) file as `profiles.corrupt-<timestamp>.json` before anything new replaces it.
    /// A hard link, so the name `profiles.json` never disappears for other instances reading without the lock
    /// (copy where links are unsupported). Returns the new file, or `nil` if there was nothing left to keep.
    @discardableResult
    static func preserveDamagedFile(at url: URL, now: Date) throws(ProfileError) -> URL? {
        let folder = url.deletingLastPathComponent()
        let stem = url.deletingPathExtension().lastPathComponent
        let stamp = ProfileDateCoding.compactTimestamp(now)
        for attempt in 0..<1000 {
            let suffix = attempt == 0 ? "" : "-\(attempt)"
            let target = folder.appending(path: "\(stem).corrupt-\(stamp)\(suffix).\(url.pathExtension)")
            if FileManager.default.fileExists(atPath: target.path) { continue }
            if link(url.path, target.path) == 0 { return target }
            switch errno {
            case ENOENT: return nil
            case EEXIST: continue
            default:
                do {
                    try FileManager.default.copyItem(at: url, to: target)
                    return target
                } catch {
                    throw .corruptFileNotPreserved
                }
            }
        }
        throw .corruptFileNotPreserved
    }
}

/// An exclusive `flock` on `profiles.json.lock`, held around every read-modify-write. The kernel drops it if the
/// process dies, and two stores in one process contend like two processes (separate open files).
final class ProfileFileLock {
    private var fd: Int32

    private init(fd: Int32) {
        self.fd = fd
    }

    /// Polls for up to `timeout` seconds; never proceeds without the lock.
    static func acquire(at url: URL, timeout: TimeInterval) throws(ProfileError) -> ProfileFileLock {
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let fd = open(url.path, O_RDWR | O_CREAT | O_CLOEXEC, 0o600)
        guard fd >= 0 else { throw .lockUnavailable }
        let deadline = Date().addingTimeInterval(timeout)
        while true {
            if flock(fd, LOCK_EX | LOCK_NB) == 0 { return ProfileFileLock(fd: fd) }
            let code = errno
            if code == EINTR { continue }
            guard code == EWOULDBLOCK || code == EAGAIN, Date() < deadline else {
                close(fd)
                throw .lockUnavailable
            }
            usleep(5_000)
        }
    }

    func release() {
        guard fd >= 0 else { return }
        flock(fd, LOCK_UN)
        close(fd)
        fd = -1
    }

    deinit {
        release()
    }
}

/// Watches the folder that holds `profiles.json` (a file watch would go stale at the first atomic replace) and calls
/// `onChange` once per burst of activity, 300 ms after the last event. Other files in the folder also trigger it; the
/// store tells real changes apart by comparing the file itself.
final class ProfileFileWatcher: @unchecked Sendable {
    private let queue = DispatchQueue(label: "app.subtitleadder.profile-watcher", qos: .utility)
    private let debounce: TimeInterval
    private let onChange: @Sendable () -> Void
    private let source: any DispatchSourceFileSystemObject
    /// Only touched on `queue`.
    private var pending: DispatchWorkItem?

    init?(folder: URL, debounce: TimeInterval = 0.3, onChange: @escaping @Sendable () -> Void) {
        let fd = open(folder.path, O_EVTONLY)
        guard fd >= 0 else { return nil }
        self.debounce = debounce
        self.onChange = onChange
        source = DispatchSource.makeFileSystemObjectSource(fileDescriptor: fd, eventMask: [.write, .rename, .delete], queue: queue)
        source.setCancelHandler { close(fd) }
        source.setEventHandler { [weak self] in self?.schedule() }
        source.resume()
    }

    private func schedule() {
        pending?.cancel()
        let item = DispatchWorkItem { [onChange] in onChange() }
        pending = item
        queue.asyncAfter(deadline: .now() + debounce, execute: item)
    }

    func stop() {
        source.cancel()
    }

    deinit {
        source.cancel()
    }
}
