# Somtus documentation

Start with the [project overview and release build](../README.md). The topic
guides below retain the detailed instructions formerly in the root README.
Commands and source paths in those guides are relative to the checkout root.

## Using Somtus

| Topic | Guide |
| --- | --- |
| Projects, worktrees, tasks and activity | [Projects and tasks](guides/projects-and-tasks.md) |
| Persistent shells, files, input and scrollback | [Terminals](guides/terminals.md) |
| Agent launches, usage, chat host and chat tabs | [Agents and chat](guides/chat.md) |
| Global and project orchestrators | [Orchestrators](guides/orchestration.md) |
| Passing context to another agent | [Handoff](guides/handoff.md) |
| Panes, tabs, focus and keyboard shortcuts | [Workspace controls](guides/interface.md) |
| Appearance, accounts, permissions and project preferences | [Settings](guides/settings.md) |
| Personal layouts, command bars and TSX add-ins | [Customization](customization.md) |
| Scheduled project chats and existing destinations | [Automations](automations.md) |
| Scheduler timing, persistence and legacy targets | [Scheduling reference](scheduling.md) |
| App previews and simulator tabs | [Preview guide](guides/previews.md), [CLI/MCP controls](preview-control.md) |
| Cua.ai Driver, macOS access and agent setup | [Computer use](guides/computer-use.md) |

## Hosts and mobile access

- [Remote access guide](guides/remote-access.md): paired phones and desktop clients.
- [iPhone and iPad setup](../ios/README.md): build, pairing, native surfaces and verification.
- [SSH hosts](ssh-hosts.md): register an existing authenticated SSH route.
- [Phone access through a desktop gateway](phone-ssh-gateway.md): use registered SSH hosts through a paired desktop.
- [Multi-host agents and recovery](multi-host-agents.md): execution identity, metadata and connection state.
- [Remote companion setup](../remote/README.md) and [relay deployment](remote-deployment.md).
- Wire contracts: [protocol v1 and additive extensions](remote-protocol.md), [protocol v2](remote-protocol-v2.md), and [shared project tabs](shared-tabs.md).

## Agents, dashboards and chat contracts

- [ACP providers](acp.md) and [terminal program status](program-status.md).
- [Durable native chat delegation](chat-delegation.md).
- [Chat event recovery](chat-events.md) and [provider notices](chat-notices.md).
- [Dashboard decisions and observed activity](dashboard-decisions.md).
- [Optional Haiku dashboard summaries](haiku-dashboard.md).
- [Codex subscription balancing](subscription-balancing.md).

## Development and architecture

- [Build, development, tests, updates and rollback](guides/development.md).
- [Desktop NSH host](../crates/riwork-desktop/README-NSH.md), [native UI APIs](../crates/riwork-nsh-native/README.md), and [Ghostty integration](../crates/riwork-nsh-terminal/README.md).
- [iOS migration boundary](nsh-ios-migration.md).
- [NSH interface contracts](nsh-contracts.md), [terminal/layout preservation specification](nsh-spec-terminal-layout.md), and [parity checklist](nsh-parity.md). These contain frozen baselines and dated decisions; historical open items are not a current acceptance verdict.
- [NSH 0.3 toolchain baseline](nsh-toolchain.md): the recorded isolated setup and its evidence. Use the development and platform guides for current build entry points.
- [Watch and polling mitigation](watch-polling-mitigation.md): retained mitigation record referenced by the code.

## Ongoing work and delivery evidence

- [Performance handover](performance-audit-handover-2026-11-06.md): pass10 is the last completed pass; pass11 remains unfinished. Preserve the [pass10 report](performance-audit-2026-11-06-pass10.txt), [earlier pass history](performance-audit-2026-11-06.txt), sibling pass reports and `target/performance-audit/` evidence. [Deployment evidence](performance-audit-deployment-2026-11-06.txt) records the installed performance scope.
- [Phone SSH gateway](phone-ssh-gateway.md): implementation has been built and tested; deployment remains pending.
- [Chat improvement roadmap](chat-improvement-roadmap.md): delivered and planned behavior with acceptance checks.
- [NSH schedules follow-up](nsh-schedules-parity-followup.md), [native preview report](nsh-native-preview.md), [Phase 6 review](nsh-phase6-review.md), and [chat image verification](verification/nsh-chat-images-6d2cc795/README.md) retain explicit limitations or review status. Do not infer closure from their presence in an archive index.

## Historical records

The [historical documentation index](archive/README.md) groups implementation
reports, phase checkpoints and verification artifacts, including the relocation
map for former root-level reports. Hash-pinned NSH documents and evidence remain
at their original paths and retain their original bytes.
