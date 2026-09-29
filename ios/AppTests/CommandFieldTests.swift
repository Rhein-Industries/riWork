import XCTest
import SwiftUI
import UIKit
@testable import RiWorkRemote

@MainActor final class CommandFieldTests: XCTestCase {
    private final class Box { var text = "" }
    private var windows: [UIWindow] = []  // keeps each hosted hierarchy alive for the test
    private func makeField(_ box: Box, submitted: @escaping () -> Void = {}, rejected: @escaping () -> Void = {}) -> UITextField {
        let view = CommandField(text: Binding(get: { box.text }, set: { box.text = $0 }), placeholder: "Continue…", isEnabled: true, label: "Command", onSubmit: submitted, onRejectedInput: rejected)
        let controller = UIHostingController(rootView: view)
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 320, height: 80))
        window.rootViewController = controller
        window.makeKeyAndVisible()
        controller.view.layoutIfNeeded()
        func find(_ view: UIView) -> UITextField? { (view as? UITextField) ?? view.subviews.lazy.compactMap(find).first }
        windows.append(window)
        return find(controller.view)!
    }
    func testTraitsKeepSmartPunctuationAndCapitalizationOff() {
        let field = makeField(Box())
        XCTAssertEqual(field.smartQuotesType, .no, "a typed \" must not become “")
        XCTAssertEqual(field.smartDashesType, .no, "--oneline must not become —oneline")
        XCTAssertEqual(field.smartInsertDeleteType, .no)
        XCTAssertEqual(field.inlinePredictionType, .no)
        XCTAssertEqual(field.mathExpressionCompletionType, .no)
        XCTAssertEqual(field.writingToolsBehavior, .none)
        XCTAssertEqual(field.autocapitalizationType, .none)
        XCTAssertEqual(field.autocorrectionType, .no)
        XCTAssertEqual(field.spellCheckingType, .no)
        XCTAssertEqual(field.returnKeyType, .send)
    }
    func testPairingCodeFieldKeepsJsonPunctuationLiteral() {
        let box = Box()
        let view = PairingCodeField(text: Binding(get: { box.text }, set: { box.text = $0 }))
        let controller = UIHostingController(rootView: view)
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 320, height: 180))
        window.rootViewController = controller
        window.makeKeyAndVisible()
        controller.view.layoutIfNeeded()
        func find(_ view: UIView) -> UITextView? { (view as? UITextView) ?? view.subviews.lazy.compactMap(find).first }
        let editor = find(controller.view)!
        windows.append(window)
        XCTAssertEqual(editor.smartQuotesType, .no)
        XCTAssertEqual(editor.smartDashesType, .no)
        XCTAssertEqual(editor.smartInsertDeleteType, .no)
        XCTAssertEqual(editor.inlinePredictionType, .no)
        XCTAssertEqual(editor.mathExpressionCompletionType, .no)
        XCTAssertEqual(editor.writingToolsBehavior, .none)
        XCTAssertEqual(editor.autocapitalizationType, .none)
        XCTAssertEqual(editor.autocorrectionType, .no)
        XCTAssertEqual(editor.spellCheckingType, .no)
        XCTAssertEqual(editor.accessibilityLabel, "Pairing JSON or deep link")
        editor.text = "{\"v\":1,\"relay_url\":\"wss://relay.example/v1/ws\"}"
        editor.delegate?.textViewDidChange?(editor)
        XCTAssertEqual(box.text, "{\"v\":1,\"relay_url\":\"wss://relay.example/v1/ws\"}")
    }
    private func appear(_ host: UIViewController) {
        host.beginAppearanceTransition(true, animated: false)
        host.endAppearanceTransition()
    }
    private func disappear(_ host: UIViewController) {
        host.beginAppearanceTransition(false, animated: false)
        host.endAppearanceTransition()
    }
    func testScannerStartsOnlyAfterItAppearsAndRestartsAfterDisappear() {
        var starts = 0
        var stops = 0
        var errors = 0
        let host = ScannerStartController(start: { starts += 1 }, stop: { stops += 1 }, onError: { _ in errors += 1 })
        host.loadViewIfNeeded()
        XCTAssertEqual(starts, 0)
        appear(host)
        XCTAssertEqual(starts, 1)
        appear(host)
        XCTAssertEqual(starts, 1, "a second appear while scanning must not start twice")
        disappear(host)
        XCTAssertEqual(stops, 1)
        appear(host)
        XCTAssertEqual(starts, 2)
        XCTAssertEqual(errors, 0)
    }
    func testScannerReportsAStartFailureWithoutMarkingItselfScanning() {
        var stops = 0
        var message = ""
        struct Boom: Error {}
        let host = ScannerStartController(start: { throw Boom() }, stop: { stops += 1 }, onError: { message = $0 })
        host.loadViewIfNeeded()
        appear(host)
        XCTAssertEqual(message, "Camera could not start. Paste the pairing code instead.")
        disappear(host)
        XCTAssertEqual(stops, 0, "a failed start has no scanning session to stop")
    }
    func testReturnSubmitsInsteadOfInsertingANewline() {
        var submitted = 0
        let field = makeField(Box(), submitted: { submitted += 1 })
        XCTAssertEqual(field.delegate?.textFieldShouldReturn?(field), false)
        XCTAssertEqual(submitted, 1)
    }
    func testMultilinePasteIsRefusedButOneTrailingNewlineIsDropped() {
        let box = Box()
        var rejected = 0
        let field = makeField(box, rejected: { rejected += 1 })
        let delegate = try! XCTUnwrap(field.delegate)
        XCTAssertEqual(delegate.textField?(field, shouldChangeCharactersIn: NSRange(location: 0, length: 0), replacementString: "git status"), true)
        XCTAssertEqual(delegate.textField?(field, shouldChangeCharactersIn: NSRange(location: 0, length: 0), replacementString: "a\nb"), false)
        XCTAssertEqual(delegate.textField?(field, shouldChangeCharactersIn: NSRange(location: 0, length: 0), replacementString: "a\u{2028}b"), false)
        XCTAssertEqual(rejected, 2)
        XCTAssertEqual(field.text ?? "", "")
        XCTAssertEqual(delegate.textField?(field, shouldChangeCharactersIn: NSRange(location: 0, length: 0), replacementString: "echo hi\n"), false)
        XCTAssertEqual(field.text, "echo hi")
        XCTAssertEqual(box.text, "echo hi")
        XCTAssertEqual(rejected, 2)
    }
}
