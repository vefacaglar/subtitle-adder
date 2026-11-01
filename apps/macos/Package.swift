// swift-tools-version: 6.0
import Foundation
import PackageDescription

// Embeds Info.plist into the binary so the app gets a name when running without a bundle (swift run, VS Code).
let infoPlist = URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("Support/Info.plist").path

let package = Package(
    name: "SubtitleAdder",
    // Turkish is the source language: catalog keys are the Turkish strings.
    defaultLocalization: "tr",
    platforms: [.macOS(.v15)],
    products: [
        .executable(name: "SubtitleAdderApp", targets: ["SubtitleAdderApp"]),
    ],
    targets: [
        // Runs the Python engine as a subprocess and turns the JSON event stream into Swift types.
        .target(name: "EngineKit", resources: [.process("Resources")]),
        .executableTarget(
            name: "SubtitleAdderApp",
            dependencies: ["EngineKit"],
            resources: [.process("Resources")],
            linkerSettings: [.unsafeFlags(["-Xlinker", "-sectcreate", "-Xlinker", "__TEXT", "-Xlinker", "__info_plist", "-Xlinker", infoPlist])]
        ),
        .testTarget(name: "EngineKitTests", dependencies: ["EngineKit"]),
        // View model behavior (undo of edits), via @testable import of the executable target.
        .testTarget(name: "SubtitleAdderAppTests", dependencies: ["SubtitleAdderApp", "EngineKit"]),
    ]
)
