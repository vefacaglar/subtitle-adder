// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "SubtitleAdder",
    platforms: [.macOS(.v15)],
    products: [
        .executable(name: "SubtitleAdderApp", targets: ["SubtitleAdderApp"]),
    ],
    targets: [
        // Python motorunu alt süreç olarak çalıştırır, JSON olay akışını Swift tiplerine çevirir.
        .target(name: "EngineKit"),
        .executableTarget(name: "SubtitleAdderApp", dependencies: ["EngineKit"]),
        .testTarget(name: "EngineKitTests", dependencies: ["EngineKit"]),
    ]
)
