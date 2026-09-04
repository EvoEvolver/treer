import AVFoundation
import SwiftUI
import UIKit

struct VoicePreviewSheet: View {
    @EnvironmentObject private var session: AppSession
    @Environment(\.dismiss) private var dismiss
    @State private var micAuthorized = false
    @StateObject private var asr = HoldToTalkAsr()

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 12) {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 10) {
                            if session.voiceLines.isEmpty, asr.liveUser.isEmpty, asr.partial.isEmpty, !session.voiceBusy {
                                Text(emptyHint)
                                    .foregroundStyle(.secondary)
                            }
                            ForEach(session.voiceLines) { line in
                                Text("\(line.role): \(line.text)")
                            }
                            if !asr.liveUser.isEmpty || !asr.partial.isEmpty {
                                Text("user: \([asr.liveUser, asr.partial].filter { !$0.isEmpty }.joined(separator: " "))")
                                    .foregroundStyle(.secondary)
                                    .id("live")
                            }
                            if session.voiceBusy {
                                Text("assistant: …")
                                    .foregroundStyle(.secondary)
                                    .id("busy")
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .onChange(of: session.voiceLines.count) { _, _ in
                        proxy.scrollTo(session.voiceLines.last?.id, anchor: .bottom)
                    }
                }
                .padding(12)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
                if !micAuthorized {
                    Button("允许麦克风") {
                        AVAudioSession.sharedInstance().requestRecordPermission { granted in
                            DispatchQueue.main.async { micAuthorized = granted }
                        }
                    }
                }
                if let error = asr.errorMessage, !error.isEmpty {
                    Text(error).font(.footnote).foregroundStyle(.red)
                }
                if session.voiceAsr.enabled, micAuthorized {
                    HoldTalkButton(holding: asr.holding) { down in
                        if down {
                            if asr.holding {
                                finishHold()
                                return
                            }
                            session.stopVoiceSpeech()
                            startAsr()
                        } else {
                            finishHold()
                        }
                    }
                    .accessibilityIdentifier("voice-hold-button")
                }
            }
            .padding(16)
            .navigationTitle("Voice")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
        }
        .accessibilityIdentifier("voice-preview")
        .onAppear {
            refreshMicStatus()
            Task { await session.refreshVoiceAsr() }
        }
        .onDisappear {
            session.stopVoiceSpeech()
            asr.shutdown()
        }
    }

    private var emptyHint: String {
        if !micAuthorized { return "需要麦克风权限。" }
        if !session.voiceAsr.enabled { return "ASR 未配置。" }
        return "按住说话"
    }

    private func finishHold() {
        guard asr.requestStop() else { return }
        Task {
            try? await Task.sleep(nanoseconds: 1_800_000_000)
            let text = asr.consumeHold()
            asr.closeSocket()
            if text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                asr.errorMessage = "没听清，请按住再说一次"
                return
            }
            await session.submitVoiceUtterance(text)
        }
    }

    private func startAsr() {
        guard let client = session.apiClient,
              let workspaceId = session.selectedWorkspace?.workspaceId,
              let request = try? client.voiceAsrWebSocketRequest(workspaceId: workspaceId)
        else {
            asr.errorMessage = "Sign in to a workspace first."
            return
        }
        asr.start(request: request)
    }

    private func refreshMicStatus() {
        micAuthorized = AVAudioSession.sharedInstance().recordPermission == .granted
    }
}

private struct HoldTalkButton: View {
    var holding: Bool
    var onHold: (Bool) -> Void

    var body: some View {
        Text(holding ? "松开结束" : "按住说话")
            .font(.headline)
            .frame(maxWidth: .infinity, minHeight: 52)
            .foregroundStyle(.white)
            .background(holding ? Color.red : Color.accentColor, in: RoundedRectangle(cornerRadius: 12))
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { _ in
                        if !holding { onHold(true) }
                    }
                    .onEnded { _ in
                        onHold(false)
                    }
            )
            .onDisappear {
                if holding { onHold(false) }
            }
    }
}

@MainActor
final class HoldToTalkAsr: ObservableObject {
    @Published var holding = false
    @Published var partial = ""
    @Published var liveUser = ""
    @Published var errorMessage: String?
    private(set) var isFinishing = false

    private var holdLines: [String] = []
    private var stopping = false
    private var generation = 0
    private var socket: URLSessionWebSocketTask?
    private var session: URLSession?
    private var capture: PcmCapture?
    private var receiveTask: Task<Void, Never>?

    func start(request: URLRequest) {
        if holding { return }
        generation += 1
        shutdownKeepingGeneration()
        stopping = false
        isFinishing = false
        errorMessage = nil
        partial = ""
        liveUser = ""
        holdLines = []
        holding = true
        let session = URLSession(configuration: .default)
        self.session = session
        let task = session.webSocketTask(with: request)
        socket = task
        task.resume()
        receiveTask = Task { [weak self] in
            await self?.receiveForever()
        }
        let capture = PcmCapture()
        self.capture = capture
        do {
            try capture.start { [weak self] data in
                self?.socket?.send(.data(data)) { _ in }
            }
        } catch {
            errorMessage = error.localizedDescription
            stop()
        }
    }

    @discardableResult
    func requestStop() -> Bool {
        if isFinishing { return false }
        if !holding { return false }
        stopping = true
        isFinishing = true
        holding = false
        capture?.stop()
        capture = nil
        if let socket {
            socket.send(.string("{\"type\":\"stop\"}")) { _ in }
        }
        return true
    }

    func closeSocket() {
        stopping = true
        receiveTask?.cancel()
        receiveTask = nil
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
        session?.invalidateAndCancel()
        session = nil
    }

    func shutdown() {
        generation += 1
        shutdownKeepingGeneration()
    }

    private func shutdownKeepingGeneration() {
        stopping = true
        isFinishing = false
        holding = false
        capture?.stop()
        capture = nil
        closeSocket()
    }

    func stop() {
        requestStop()
        closeSocket()
    }

    private func receiveForever() async {
        while !Task.isCancelled {
            guard let socket else { return }
            do {
                let message = try await socket.receive()
                let text: String?
                switch message {
                case let .string(value):
                    text = value
                case let .data(data):
                    text = String(data: data, encoding: .utf8)
                @unknown default:
                    text = nil
                }
                if let text {
                    handle(text)
                }
            } catch {
                if Task.isCancelled || stopping { return }
                let message = error.localizedDescription.lowercased()
                if message.contains("closed") || message.contains("cancel") || message.contains("eof") {
                    return
                }
                errorMessage = error.localizedDescription
                holding = false
                return
            }
        }
    }

    private func handle(_ text: String) {
        guard let data = text.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let type = object["type"] as? String
        else { return }
        switch type {
        case "partial":
            partial = object["text"] as? String ?? ""
        case "final":
            let line = object["text"] as? String ?? ""
            if !line.isEmpty {
                holdLines.append(line)
                liveUser = holdLines.joined(separator: " ")
            }
            partial = ""
        case "error":
            if !stopping {
                errorMessage = object["message"] as? String ?? "ASR failed"
            }
        default:
            break
        }
    }

    func consumeHold() -> String {
        let joined = holdLines.joined(separator: " ").trimmingCharacters(in: .whitespacesAndNewlines)
        let extra = partial.trimmingCharacters(in: .whitespacesAndNewlines)
        holdLines.removeAll()
        liveUser = ""
        partial = ""
        if !joined.isEmpty, !extra.isEmpty, !joined.contains(extra) {
            return "\(joined) \(extra)"
        }
        return joined.isEmpty ? extra : joined
    }
}

private final class PcmCapture {
    private let engine = AVAudioEngine()

    func start(onChunk: @escaping (Data) -> Void) throws {
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.playAndRecord, mode: .voiceChat, options: [.defaultToSpeaker, .allowBluetoothHFP])
        try session.setPreferredSampleRate(16_000)
        try session.setActive(true, options: [])
        let input = engine.inputNode
        let hwFormat = input.outputFormat(forBus: 0)
        guard let outFormat = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 16_000, channels: 1, interleaved: true) else {
            throw APIError(status: -1, message: "Unable to create 16 kHz PCM format.", code: nil)
        }
        input.removeTap(onBus: 0)
        input.installTap(onBus: 0, bufferSize: 1600, format: hwFormat) { buffer, _ in
            guard let converter = AVAudioConverter(from: hwFormat, to: outFormat) else { return }
            let ratio = outFormat.sampleRate / hwFormat.sampleRate
            let frames = AVAudioFrameCount((Double(buffer.frameLength) * ratio).rounded(.up) + 16)
            guard let converted = AVAudioPCMBuffer(pcmFormat: outFormat, frameCapacity: frames) else { return }
            var error: NSError?
            var consumed = false
            converter.convert(to: converted, error: &error) { _, status in
                if consumed {
                    status.pointee = .endOfStream
                    return nil
                }
                consumed = true
                status.pointee = .haveData
                return buffer
            }
            guard error == nil, let channels = converted.int16ChannelData else { return }
            let byteCount = Int(converted.frameLength) * MemoryLayout<Int16>.size
            onChunk(Data(bytes: channels[0], count: byteCount))
        }
        engine.prepare()
        try engine.start()
    }

    func stop() {
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
    }
}
