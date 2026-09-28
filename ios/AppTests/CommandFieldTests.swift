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
        XCTAssertEqual(field.autocapitalizationType, .none)
        XCTAssertEqual(field.autocorrectionType, .no)
        XCTAssertEqual(field.spellCheckingType, .no)
        XCTAssertEqual(field.returnKeyType, .send)
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
