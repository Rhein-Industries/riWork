// Offline Mermaid layout/rasterization worker. No app windows, disk pages, or
// persistent website data. The parent kills this process on cancellation/timeout.
import AppKit
import WebKit

struct Request: Decodable {
    let source: String
    let bundle: String
    let renderer: String
    let dark: Bool
    let rules: String
}

func finish(_ value: [String: Any]) -> Never {
    let data = (try? JSONSerialization.data(withJSONObject: value)) ?? Data("{\"error\":\"Renderer failed.\"}".utf8)
    FileHandle.standardOutput.write(data)
    exit(0)
}

func quoted(_ value: Any) -> String {
    String(data: try! JSONSerialization.data(withJSONObject: value, options: [.fragmentsAllowed]), encoding: .utf8)!
}

final class Renderer: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
    let request: Request
    var web: WKWebView!
    init(_ request: Request) {
        self.request = request
        super.init()
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        config.preferences.javaScriptCanOpenWindowsAutomatically = false
        config.userContentController.add(self, name: "result")
        web = WKWebView(frame: NSRect(x: 0, y: 0, width: 1200, height: 900), configuration: config)
        web.navigationDelegate = self
    }
    func start() {
        // Blocks subresources independently of Mermaid's strict configuration.
        let rules = "[{\"trigger\":{\"url-filter\":\".*\"},\"action\":{\"type\":\"block\"}}]"
        let ruleCache = URL(fileURLWithPath: request.rules, isDirectory: true)
        WKContentRuleListStore(url: ruleCache).compileContentRuleList(forIdentifier: "riwork-diagram-offline", encodedContentRuleList: rules) { [self] list, error in
            guard let list = list, error == nil else { finish(["error": "Could not isolate the diagram renderer."]) }
            web.configuration.userContentController.add(list)
            web.loadHTMLString("""
            <!doctype html><meta charset="utf-8">
            <meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src 'none'; connect-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'">
            <style>html,body{margin:0;font:14px Arial,sans-serif}body{width:1200px}</style><body></body>
            """, baseURL: nil)
        }
    }
    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction, decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        decisionHandler(action.request.url?.absoluteString == "about:blank" ? .allow : .cancel)
    }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        webView.evaluateJavaScript(request.bundle) { [self] _, error in
            guard error == nil else { finish(["error": "Could not load the bundled Mermaid renderer."]) }
            let call = request.renderer + "\nriworkRender(" + quoted(request.source) + "," + (request.dark ? "true" : "false") + ");void 0;"
            webView.evaluateJavaScript(call) { _, error in
                if error != nil { finish(["error": "Could not start diagram rendering."]) }
            }
        }
    }
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        finish(["error": "The offline diagram page failed to load."])
    }
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        finish(["error": "The offline diagram page failed to load."])
    }
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        finish(["error": "The diagram renderer stopped unexpectedly."])
    }
    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let result = message.body as? [String: Any] else { finish(["error": "Invalid diagram renderer response."]) }
        finish(result)
    }
}

var data = Data()
while let chunk = try? FileHandle.standardInput.read(upToCount: 64 * 1024), !chunk.isEmpty {
    data.append(chunk)
    if data.count > 5 * 1024 * 1024 { finish(["error": "Diagram request is too large."]) }
}
guard let request = try? JSONDecoder().decode(Request.self, from: data), request.source.utf8.count <= 32 * 1024 else {
    finish(["error": "Invalid or oversized diagram request."])
}
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let renderer = Renderer(request)
renderer.start()
DispatchQueue.main.asyncAfter(deadline: .now() + 12) {
    finish(["error": "Diagram rendering took too long. Source is still available."])
}
app.run()
