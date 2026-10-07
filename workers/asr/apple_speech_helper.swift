import AVFAudio
import CoreMedia
import Foundation
import Speech

struct WorkerRequest: Decodable {
    let contract_version: Int
    let adapter_id: String
    let media_path: String
    let media_duration_ms: Int64
    let model_id: String
    let language: String?
    let prompt: String?
}

struct Segment: Encodable {
    let start_ms: Int64
    let end_ms: Int64
    let text: String
    let confidence: Double?
}

struct WorkerResult: Encodable {
    let contract_version: Int
    let adapter_id: String
    let model_id: String
    let language: String?
    let segments: [Segment]
    let notes: [String]
}

enum HelperError: Error, CustomStringConvertible {
    case unsupportedOS
    case unsupportedLocale(String)
    case emptyTranscript

    var description: String {
        switch self {
        case .unsupportedOS:
            return "SpeechAnalyzer requires macOS 26 or newer"
        case .unsupportedLocale(let value):
            return "SpeechTranscriber does not support locale \(value)"
        case .emptyTranscript:
            return "SpeechAnalyzer produced no finalized transcript segments"
        }
    }
}

@main
struct TsubameAppleSpeechHelper {
    static func main() async {
        do {
            guard #available(macOS 26.0, *) else {
                throw HelperError.unsupportedOS
            }
            let data = FileHandle.standardInput.readDataToEndOfFile()
            let request = try JSONDecoder().decode(WorkerRequest.self, from: data)
            guard request.contract_version == 1 else {
                throw NSError(
                    domain: "TsubameAppleSpeech",
                    code: 2,
                    userInfo: [NSLocalizedDescriptionKey: "unsupported ASR contract version"]
                )
            }

            let result = try await transcribe(request)
            let output = try JSONEncoder().encode(result)
            FileHandle.standardOutput.write(output)
        } catch {
            FileHandle.standardError.write(
                Data((String(describing: error) + "\n").utf8)
            )
            exit(2)
        }
    }

    @available(macOS 26.0, *)
    static func transcribe(_ request: WorkerRequest) async throws -> WorkerResult {
        let localeIdentifier: String
        switch request.language {
        case nil, "", "auto":
            localeIdentifier = Locale.current.identifier
        case "ja":
            localeIdentifier = "ja_JP"
        case "zh":
            localeIdentifier = "zh_CN"
        case "yue":
            localeIdentifier = "zh_HK"
        case "en":
            localeIdentifier = "en_US"
        case "ko":
            localeIdentifier = "ko_KR"
        case .some(let value):
            localeIdentifier = value
        }

        let locale = Locale(identifier: localeIdentifier)
        let supported = await SpeechTranscriber.supportedLocales
        let supportedIDs = Set(supported.map { $0.identifier(.bcp47) })
        guard supportedIDs.contains(locale.identifier(.bcp47)) else {
            throw HelperError.unsupportedLocale(localeIdentifier)
        }

        let transcriber = SpeechTranscriber(
            locale: locale,
            transcriptionOptions: [],
            reportingOptions: [],
            attributeOptions: [.audioTimeRange]
        )

        let installed = await Set(SpeechTranscriber.installedLocales)
        let installedIDs = Set(installed.map { $0.identifier(.bcp47) })
        if !installedIDs.contains(locale.identifier(.bcp47)),
           let downloader = try await AssetInventory.assetInstallationRequest(
               supporting: [transcriber]
           ) {
            try await downloader.downloadAndInstall()
        }

        let file = try AVAudioFile(
            forReading: URL(fileURLWithPath: request.media_path)
        )
        let analyzer = SpeechAnalyzer(modules: [transcriber])

        let resultTask = Task { () throws -> [Segment] in
            var segments: [Segment] = []
            for try await result in transcriber.results {
                guard result.isFinal else { continue }
                let text = String(result.text.characters).trimmingCharacters(
                    in: .whitespacesAndNewlines
                )
                guard !text.isEmpty else { continue }

                let startSeconds = CMTimeGetSeconds(result.range.start)
                let durationSeconds = CMTimeGetSeconds(result.range.duration)
                guard startSeconds.isFinite, durationSeconds.isFinite else {
                    continue
                }

                segments.append(
                    Segment(
                        start_ms: Int64((startSeconds * 1000).rounded()),
                        end_ms: Int64(((startSeconds + durationSeconds) * 1000).rounded()),
                        text: text,
                        confidence: nil
                    )
                )
            }
            return segments
        }

        if let lastSample = try await analyzer.analyzeSequence(from: file) {
            try await analyzer.finalizeAndFinish(through: lastSample)
        } else {
            await analyzer.cancelAndFinishNow()
        }

        let segments = try await resultTask.value
        guard !segments.isEmpty else {
            throw HelperError.emptyTranscript
        }

        return WorkerResult(
            contract_version: 1,
            adapter_id: "apple-speech",
            model_id: "speech-transcriber",
            language: locale.identifier(.bcp47),
            segments: segments,
            notes: ["On-device Apple SpeechAnalyzer / SpeechTranscriber"]
        )
    }
}
