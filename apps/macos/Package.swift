// swift-tools-version: 6.0
import Foundation
import PackageDescription

// Bundle'sız çalışan uygulamaya (swift run, VS Code) ad vermek için Info.plist'i ikiliye gömer.
let infoPlist = URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("Support/Info.plist").path

let package = Package(
    name: "SubtitleAdder",
    platforms: [.macOS(.v15)],
    products: [
        .executable(name: "SubtitleAdderApp", targets: ["SubtitleAdderApp"]),
    ],
    targets: [
        // Python motorunu alt süreç olarak çalıştırır, JSON olay akışını Swift tiplerine çevirir.
        .target(name: "EngineKit"),
        .executableTarget(
            name: "SubtitleAdderApp",
            dependencies: ["EngineKit"],
            linkerSettings: [.unsafeFlags(["-Xlinker", "-sectcreate", "-Xlinker", "__TEXT", "-Xlinker", "__info_plist", "-Xlinker", infoPlist])]
        ),
        .testTarget(name: "EngineKitTests", dependencies: ["EngineKit"]),
    ]
)
