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

private struct ScannerView: UIViewControllerRepresentable {
    var onScan: (String) -> Void
    var onError: (String) -> Void
    func makeCoordinator() -> Coordinator { Coordinator(onScan: onScan, onError: onError) }
    func makeUIViewController(context: Context) -> DataScannerViewController {
        let scanner = DataScannerViewController(recognizedDataTypes: [.barcode(symbologies: [.qr])], qualityLevel: .balanced, recognizesMultipleItems: false, isGuidanceEnabled: true, isHighlightingEnabled: true)
        scanner.delegate = context.coordinator
        do { try scanner.startScanning() } catch { Task { @MainActor in onError("Camera could not start. Paste the pairing code instead.") } }
        return scanner
    }
    func updateUIViewController(_ uiViewController: DataScannerViewController, context: Context) {}
    static func dismantleUIViewController(_ uiViewController: DataScannerViewController, coordinator: Coordinator) { uiViewController.stopScanning() }
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
