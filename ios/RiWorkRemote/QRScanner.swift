import SwiftUI
import VisionKit
import AVFoundation

struct QRScannerSheet: View {
    var onScan: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var ready = false
    @State private var message: String?
    var body: some View {
        NavigationStack {
            Group {
                if ready { ScannerView(onScan: { value in onScan(value); dismiss() }, onError: { message = $0; ready = false }) }
                else { ContentUnavailableView("Camera unavailable", systemImage: "qrcode.viewfinder", description: Text(message ?? "Checking camera access… You can also paste the pairing code.")) }
            }
            .navigationTitle("Scan desktop code")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } } }
            .task {
                guard DataScannerViewController.isSupported else { message = "QR scanning requires a supported iPhone or iPad. Paste the pairing code on this device."; return }
                let allowed = await AVCaptureDevice.requestAccess(for: .video)
                ready = allowed && DataScannerViewController.isAvailable
                if !ready { message = "Enable camera access in Settings, or paste the pairing code." }
            }
        }
    }
}

/// DataScanner's `startScanning` throws until the view is in a window. Starting from
/// `makeUIViewController` fails on a physical device and the sheet then stays on the paste fallback.
@MainActor final class ScannerStartController: UIViewController {
    private let start: () throws -> Void
    private let stop: () -> Void
    private let onError: (String) -> Void
    private var scanning = false
    init(start: @escaping () throws -> Void, stop: @escaping () -> Void, onError: @escaping (String) -> Void) {
        self.start = start
        self.stop = stop
        self.onError = onError
        super.init(nibName: nil, bundle: nil)
    }
    @available(*, unavailable) required init?(coder: NSCoder) { nil }
    func embed(_ child: UIViewController) {
        addChild(child)
        child.view.frame = view.bounds
        child.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        view.addSubview(child.view)
        child.didMove(toParent: self)
    }
    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        guard !scanning else { return }
        do {
            try start()
            scanning = true
        } catch {
            onError("Camera could not start. Paste the pairing code instead.")
        }
    }
    override func viewDidDisappear(_ animated: Bool) {
        super.viewDidDisappear(animated)
        stopIfNeeded()
    }
    func stopIfNeeded() {
        guard scanning else { return }
        scanning = false
        stop()
    }
}

private struct ScannerView: UIViewControllerRepresentable {
    var onScan: (String) -> Void
    var onError: (String) -> Void
    func makeCoordinator() -> Coordinator { Coordinator(onScan: onScan, onError: onError) }
    func makeUIViewController(context: Context) -> ScannerStartController {
        let scanner = DataScannerViewController(recognizedDataTypes: [.barcode(symbologies: [.qr])], qualityLevel: .balanced, recognizesMultipleItems: false, isGuidanceEnabled: true, isHighlightingEnabled: true)
        scanner.delegate = context.coordinator
        let coordinator = context.coordinator
        // startScanning throws unless this controller is already in a window, so the host waits for viewDidAppear.
        let host = ScannerStartController(start: { try scanner.startScanning() }, stop: { scanner.stopScanning() }, onError: { coordinator.onError($0) })
        host.embed(scanner)
        return host
    }
    func updateUIViewController(_ uiViewController: ScannerStartController, context: Context) {}
    static func dismantleUIViewController(_ uiViewController: ScannerStartController, coordinator: Coordinator) { uiViewController.stopIfNeeded() }
    @MainActor final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        let onScan: (String) -> Void
        let onError: (String) -> Void
        var delivered = false
        init(onScan: @escaping (String) -> Void, onError: @escaping (String) -> Void) { self.onScan = onScan; self.onError = onError }
        func dataScanner(_ dataScanner: DataScannerViewController, didAdd addedItems: [RecognizedItem], allItems: [RecognizedItem]) {
            guard !delivered else { return }
            for item in addedItems {
                if case .barcode(let code) = item, let value = code.payloadStringValue { delivered = true; dataScanner.stopScanning(); onScan(value); break }
            }
        }
        func dataScanner(_ dataScanner: DataScannerViewController, becameUnavailableWithError error: DataScannerViewController.ScanningUnavailable) { onError("Camera became unavailable. Paste the pairing code instead.") }
    }
}
