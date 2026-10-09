import Foundation
import RiWorkCore

// What dictation is biased towards: the names around the session on screen and the words on its screen. Built from what the phone
// already holds, each time a dictation starts, and kept on the phone.
extension RemoteModel {
    /// The chat on screen, or else the terminal: its names, paths and recent text.
    func speechSources() -> SpeechVocabularySources {
        let project = projects.first { $0.id == projectID }
        var names = [desktop?.name, project?.name].compactMap { $0 }
        names += worktrees.filter { $0.project_id == projectID }.map(\.branch)
        names += chats.map(\.title)
        var paths = [project?.root].compactMap { $0 } + worktrees.filter { $0.project_id == projectID }.map(\.path)
        if let chatID = selectedChatID, let chat = chats.first(where: { $0.id == chatID }) {
            names += [chat.provider.title, chat.model].compactMap { $0 }
            paths.append(chat.cwd)
            let items = chatConversations[chatID]?.transcript.items.suffix(40) ?? []
            return SpeechVocabularySources(names: names, paths: paths, text: items.map { Self.speechText($0.body) }.joined(separator: "\n"))
        }
        names += sessions.map(\.title)
        if let session { paths.append(session.cwd) }
        return SpeechVocabularySources(names: names, paths: paths, text: output)
    }

    /// The words of a transcript item that a person might say back: messages, commands, file names and tools.
    static func speechText(_ body: ChatItemBody) -> String {
        switch body {
        case .userMessage(let text), .agentMessage(let text): text
        case .command(let command, _, _, _): command
        case .fileChange(let changes): changes.map(\.path).joined(separator: " ")
        case .toolCall(_, let tool, _, _): tool
        case .plan(_, let steps), .todo(let steps): steps.map(\.text).joined(separator: " ")
        case .reasoning, .webSearch, .compaction, .notice, .elided: ""
        }
    }
}
