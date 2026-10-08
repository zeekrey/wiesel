// Native macOS UI smoke test. No HTTP client, credentials, clipboard, or private app IPC.
import Cocoa
import ApplicationServices

struct Failure: Error {
    let message: String
    let code: Int32
    init(_ message: String, code: Int32 = 2) {
        self.message = message
        self.code = code
    }
}

struct Options {
    enum Mode { case preflight, live, requestPermission, selfTest, help }
    var mode: Mode
    var app: URL
    var prompt: String
    var expected: String
    var timeout: TimeInterval

    static func parse(_ args: [String]) throws -> Options {
        var mode: Mode?
        var app = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
            .appendingPathComponent("dist/Wiesel.app")
        let marker = "WIESEL_SMOKE_" + UUID().uuidString.replacingOccurrences(of: "-", with: "")
        var prompt: String?
        var expected: String?
        var timeout: TimeInterval = 150
        var index = 0
        while index < args.count {
            let argument = args[index]
            let selected: Mode?
            switch argument {
            case "--preflight": selected = .preflight
            case "--live": selected = .live
            case "--request-permission": selected = .requestPermission
            case "--self-test": selected = .selfTest
            case "--help", "-h": selected = .help
            default: selected = nil
            }
            if let selected = selected {
                guard mode == nil else { throw Failure("Choose exactly one mode.") }
                mode = selected
            } else {
                guard ["--app", "--prompt", "--expect", "--timeout"].contains(argument),
                      index + 1 < args.count else { throw Failure("Unknown option or missing option value. Use --help.") }
                index += 1
                let value = args[index]
                switch argument {
                case "--app": app = URL(fileURLWithPath: value)
                case "--prompt": prompt = value
                case "--expect": expected = value
                case "--timeout":
                    guard let seconds = Double(value), seconds.isFinite,
                          seconds >= 1, seconds <= 600 else { throw Failure("Timeout must be between 1 and 600 seconds.") }
                    timeout = seconds
                default: break
                }
            }
            index += 1
        }
        guard let mode = mode else { throw Failure("Choose --preflight or --live. No request was sent. Use --help.") }
        guard (prompt == nil) == (expected == nil) else { throw Failure("Custom --prompt and --expect must be supplied together.") }
        let finalPrompt = prompt ?? "Reply with only this exact text, without quotes or punctuation: \(marker)"
        let finalExpected = expected ?? marker
        guard !finalPrompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              finalPrompt.utf8.count <= 4000,
              !finalPrompt.contains("\n"), !finalPrompt.contains("\r"),
              !finalExpected.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw Failure("Use a nonempty single-line prompt (at most 4000 bytes) and a nonempty expected reply.")
        }
        return Options(mode: mode, app: app.standardizedFileURL.resolvingSymlinksInPath(),
                       prompt: finalPrompt, expected: finalExpected, timeout: timeout)
    }
}

struct Observation {
    let id: String
    let value: String
    let help: String
}

struct Node {
    let element: AXUIElement
    let observation: Observation
}

func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success else { return nil }
    return value
}

func stringAttribute(_ element: AXUIElement, _ name: String) -> String {
    return attribute(element, name) as? String ?? ""
}

// Traverse only the selected process; bound work and avoid cycles/duplicate windows.
func snapshot(_ app: AXUIElement) throws -> [Node] {
    var queue: [(AXUIElement, Int)] = [(app, 0)]
    var visited: [AXUIElement] = []
    var nodes: [Node] = []
    var index = 0
    while index < queue.count {
        let (element, depth) = queue[index]
        index += 1
        if visited.contains(where: { CFEqual($0, element) }) { continue }
        guard visited.count < 2000, depth <= 40 else { throw Failure("Accessibility tree exceeded traversal limits.") }
        visited.append(element)
        let id = stringAttribute(element, "AXIdentifier")
        if id.hasPrefix("wiesel.") {
            nodes.append(Node(element: element, observation: Observation(
                id: id, value: stringAttribute(element, kAXValueAttribute),
                help: stringAttribute(element, kAXHelpAttribute))))
        }
        for key in [kAXChildrenAttribute, kAXWindowsAttribute] {
            if let children = attribute(element, key) as? [AXUIElement] {
                queue.append(contentsOf: children.map { ($0, depth + 1) })
            }
        }
    }
    return nodes
}

func unique(_ nodes: [Node], _ id: String) throws -> Node? {
    let matches = nodes.filter { $0.observation.id == id }
    guard matches.count <= 1 else { throw Failure("Duplicate Accessibility identifier: \(id)") }
    return matches.first
}

func idleAndSignedIn(_ observations: [Observation]) -> Bool {
    return observations.contains { $0.id.hasPrefix("wiesel.page.") && $0.help == "Signed in; request idle" }
}

func hasConversation(_ observations: [Observation]) -> Bool {
    return observations.contains { $0.id.hasPrefix("wiesel.message.") }
}

enum ReplyOutcome: Equatable { case waiting, passed, mismatch }
func replyOutcome(_ observations: [Observation], prompt: String, expected: String) -> ReplyOutcome {
    guard idleAndSignedIn(observations),
          observations.contains(where: { $0.id == "wiesel.message.user.0" && $0.value == prompt }),
          let reply = observations.first(where: { $0.id == "wiesel.message.assistant.1" }) else { return .waiting }
    return reply.value.trimmingCharacters(in: .whitespacesAndNewlines)
        == expected.trimmingCharacters(in: .whitespacesAndNewlines) ? .passed : .mismatch
}

func pollPause() {
    RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.1))
}

func waitFor(_ seconds: TimeInterval, failure: String, code: Int32 = 2,
             condition: () throws -> Bool) throws {
    let deadline = ProcessInfo.processInfo.systemUptime + seconds
    repeat {
        if try condition() { return }
        pollPause()
    } while ProcessInfo.processInfo.systemUptime < deadline
    throw Failure(failure, code: code)
}

func launch(_ url: URL) throws -> NSRunningApplication {
    guard let bundle = Bundle(url: url), let executable = bundle.executableURL,
          FileManager.default.isExecutableFile(atPath: executable.path),
          bundle.bundleIdentifier == "com.wiesel.launcher" else {
        throw Failure("No valid Wiesel bundle at --app. Build it with bash scripts/build-app.sh first.")
    }
    let sameIdentity = NSWorkspace.shared.runningApplications.filter { $0.bundleIdentifier == bundle.bundleIdentifier }
    let matches = sameIdentity.filter {
        $0.executableURL?.resolvingSymlinksInPath() == executable.resolvingSymlinksInPath()
    }
    guard sameIdentity.count == matches.count, matches.count <= 1 else {
        throw Failure("Another Wiesel copy or multiple instances are running. Quit them manually before testing.")
    }
    if let running = matches.first {
        running.activate(options: [])
        return running
    }
    let config = NSWorkspace.OpenConfiguration()
    config.activates = true
    config.createsNewApplicationInstance = true
    var launched: NSRunningApplication?
    var failed = false
    NSWorkspace.shared.openApplication(at: url, configuration: config) { app, error in
        launched = app
        failed = error != nil
    }
    try waitFor(15, failure: "Wiesel launch timed out.") { launched != nil || failed }
    guard let running = launched, !failed else { throw Failure("Could not launch the selected Wiesel bundle.") }
    guard running.executableURL?.resolvingSymlinksInPath() == executable.resolvingSymlinksInPath() else {
        throw Failure("macOS launched an unexpected executable; refusing to interact.")
    }
    return running
}

func press(_ node: Node, submission: Bool = false) throws {
    guard AXUIElementPerformAction(node.element, kAXPressAction as CFString) == .success else {
        if submission {
            throw Failure("Send did not complete through Accessibility. The submission outcome is unknown and may have been charged. Do not retry blindly.", code: 1)
        }
        throw Failure("Accessibility press failed for \(node.observation.id). No automatic retry was attempted.")
    }
}

func run(_ options: Options) throws {
    guard AXIsProcessTrusted() else {
        throw Failure("Accessibility permission is missing. Run --request-permission, allow your terminal/runner in System Settings → Privacy & Security → Accessibility, then rerun. Runner: \(CommandLine.arguments[0])")
    }
    let running = try launch(options.app)
    let app = AXUIElementCreateApplication(running.processIdentifier)
    AXUIElementSetMessagingTimeout(app, 1)
    print("Wiesel launched/activated (PID \(running.processIdentifier)).")
    var nodes: [Node] = []
    try waitFor(15, failure: "Wiesel Accessibility selectors were not found. Rebuild and relaunch the bundle with the current source.") {
        guard !running.isTerminated else { throw Failure("Wiesel exited before its UI was ready.") }
        nodes = try snapshot(app)
        return nodes.contains { $0.observation.id.hasPrefix("wiesel.page.") }
    }
    if options.mode == .preflight {
        let observations = nodes.map(\.observation)
        print("Accessibility: available")
        print("Page: \(observations.first { $0.id.hasPrefix("wiesel.page.") }?.id ?? "unknown")")
        print("Signed in and idle: \(idleAndSignedIn(observations))")
        print("Existing conversation: \(hasConversation(observations))")
        print("Selectors: \(observations.map(\.id).sorted().joined(separator: ", "))")
        print("Preflight complete. No chat message sent; launching may perform normal session/catalog requests.")
        return
    }
    try waitFor(15, failure: "Wiesel is not signed in and idle. Finish login/model setup or wait for the current request, then rerun.") {
        nodes = try snapshot(app)
        return idleAndSignedIn(nodes.map(\.observation))
    }
    if let chat = try unique(nodes, "wiesel.action.chat") {
        try press(chat)
    } else if try unique(nodes, "wiesel.page.chat") == nil {
        throw Failure("Open Wiesel's launcher or an empty Chat before testing. Settings/Writing are not navigated automatically.")
    }
    try waitFor(5, failure: "Chat did not open or its composer is not accessible.") {
        nodes = try snapshot(app)
        return try unique(nodes, "wiesel.page.chat") != nil && unique(nodes, "wiesel.chat.composer") != nil
    }
    guard !hasConversation(nodes.map(\.observation)) else {
        throw Failure("Chat already contains messages. Choose New chat manually, then rerun. Nothing was cleared or sent.")
    }
    guard let composer = try unique(nodes, "wiesel.chat.composer"), composer.observation.value.isEmpty else {
        throw Failure("Composer contains a draft or is unavailable. Clear it manually; nothing was overwritten or sent.")
    }
    guard AXUIElementSetAttributeValue(composer.element, kAXValueAttribute as CFString,
                                      options.prompt as CFString) == .success else {
        throw Failure("Could not enter the prompt through Accessibility. No request was submitted.")
    }
    try waitFor(5, failure: "Composer did not reflect the entered prompt. No request was submitted.") {
        nodes = try snapshot(app)
        return try unique(nodes, "wiesel.chat.composer")?.observation.value == options.prompt
    }
    guard idleAndSignedIn(nodes.map(\.observation)), !hasConversation(nodes.map(\.observation)),
          let send = try unique(nodes, "wiesel.chat.send") else {
        throw Failure("Chat changed before submission. The test prompt remains as a draft; no request was submitted by the runner.")
    }
    print("Submitting one live chat request. This may consume allowance; no retries.")
    try press(send, submission: true)
    try waitFor(options.timeout, failure: "No completed assistant reply was observed. Inspect Wiesel's status bar. The request may have been charged; do not retry blindly.", code: 1) {
        guard !running.isTerminated else { throw Failure("Wiesel exited after submission. The request may have been charged.", code: 1) }
        let observations = try snapshot(app).map(\.observation)
        switch replyOutcome(observations, prompt: options.prompt, expected: options.expected) {
        case .waiting: return false
        case .mismatch: throw Failure("A completed assistant reply did not match --expect (after trimming surrounding whitespace). Reply contents are not logged.", code: 1)
        case .passed:
            guard observations.contains(where: { $0.id == "wiesel.chat.composer" && $0.value.isEmpty }) else {
                throw Failure("Reply matched, but the composer was not empty after submission.", code: 1)
            }
            return true
        }
    }
    print("PASS: user prompt displayed, completed assistant reply matched, request idle, composer empty.")
}

func selfTest() throws {
    func check(_ condition: Bool, _ name: String) throws {
        guard condition else { throw Failure("Self-test failed: \(name)", code: 1) }
    }
    func rejects(_ args: [String]) -> Bool {
        do { _ = try Options.parse(args); return false } catch { return true }
    }
    try check(rejects([]), "no implicit live mode")
    try check(rejects(["--live", "--preflight"]), "conflicting modes")
    try check(rejects(["--live", "--timeout", "nan"]), "nonfinite timeout")
    try check(rejects(["--live", "--timeout", "0"]), "zero timeout")
    try check(rejects(["--live", "--prompt", "hello"]), "paired prompt/expect")
    try check(rejects(["--live", "--prompt", "a\nb", "--expect", "ok"]), "single-line composer")
    try check(rejects(["--live", "--expect", " ", "--prompt", "hello"]), "empty expectation")
    try check(rejects(["--live", "--unknown"]), "unknown option")
    let custom = try Options.parse(["--live", "--prompt", "hello", "--expect", "ok", "--timeout", "30"])
    try check(custom.prompt == "hello" && custom.expected == "ok" && custom.timeout == 30, "custom options")
    let first = try Options.parse(["--live"])
    let second = try Options.parse(["--live"])
    try check(first.expected != second.expected && first.prompt.contains(first.expected), "unique default marker")
    let ready = Observation(id: "wiesel.page.chat", value: "", help: "Signed in; request idle")
    let user = Observation(id: "wiesel.message.user.0", value: "hello", help: "")
    let reply = Observation(id: "wiesel.message.assistant.1", value: " ok\n", help: "")
    try check(replyOutcome([ready, user, reply], prompt: "hello", expected: "ok") == .passed, "completed reply")
    try check(replyOutcome([ready, user, reply], prompt: "hello", expected: "no") == .mismatch, "mismatch")
    let stream = Observation(id: "wiesel.message.streaming.1", value: "ok", help: "")
    try check(replyOutcome([ready, user, stream], prompt: "hello", expected: "ok") == .waiting, "partial stream cannot pass")
    let busy = Observation(id: "wiesel.page.chat", value: "", help: "Signed in; request pending")
    try check(replyOutcome([busy, user, reply], prompt: "hello", expected: "ok") == .waiting, "busy cannot pass")
    try check(replyOutcome([ready, user, reply], prompt: "different", expected: "ok") == .waiting, "prompt must match")
    try check(hasConversation([stream]), "stream counts as existing history")
    try check(!hasConversation([ready]), "empty chat")
    let signedOut = Observation(id: "wiesel.page.chat", value: "", help: "Signed out; request idle")
    try check(replyOutcome([signedOut, user, reply], prompt: "hello", expected: "ok") == .waiting, "signed out cannot pass")
    try check(replyOutcome([ready, user], prompt: "hello", expected: "hello") == .waiting, "user echo cannot pass")
    print("PASS: 19 runner self-tests. No app launched or request sent.")
}

let help = """
Usage: bash scripts/ui-smoke.sh MODE [options]
  --preflight           Launch/activate the bundle and inspect Accessibility; no message sent.
  --request-permission  Ask macOS for Accessibility authorization; no app launch or request.
  --live                Open an empty Chat, submit ONE live prompt, assert the completed reply.
  --self-test           Test argument validation and reply assertions without permissions/network.
  --app PATH            Bundle to test (default: this repo's dist/Wiesel.app).
  --prompt TEXT         Custom single-line prompt; requires --expect.
  --expect TEXT         Exact expected reply, ignoring surrounding whitespace.
  --timeout SECONDS     Reply deadline, 1–600 seconds (default: 150).

Default live prompt requests a fresh unique marker. Finish login and model setup manually.
Existing history/drafts are never cleared. Leave Wiesel alone during the run.
The runner does not quit Wiesel, change settings, read credentials, or touch the clipboard.
Live calls may be billable. Failure never triggers a retry. No message contents are logged.
Exit codes: 0 success, 1 reply/assertion failure, 2 permission/setup/usage failure.
"""

do {
    let options = try Options.parse(Array(CommandLine.arguments.dropFirst()))
    switch options.mode {
    case .help: print(help)
    case .selfTest: try selfTest()
    case .requestPermission:
        let key = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
        let trusted = AXIsProcessTrustedWithOptions([key: true] as CFDictionary)
        print("Accessibility trusted: \(trusted). Runner: \(CommandLine.arguments[0])")
        if !trusted {
            print("Allow the terminal/runner in System Settings → Privacy & Security → Accessibility, then rerun --preflight.")
            exit(2)
        }
    case .preflight, .live: try run(options)
    }
} catch let error as Failure {
    fputs("FAIL: \(error.message)\n", stderr)
    exit(error.code)
} catch {
    fputs("FAIL: unexpected runner error (details suppressed).\n", stderr)
    exit(2)
}
