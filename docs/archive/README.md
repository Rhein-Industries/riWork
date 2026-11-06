# Historical implementation and verification records

Use the [documentation index](../README.md) for current guides and contracts.
These records describe particular commits, fixtures and verification runs.
Archiving or indexing a report does not resolve its open findings, convert a
partial result into acceptance, or establish that a feature was deployed.

## Former root-level paths

Frozen documents, including `nsh-parity.md` and `nsh-phase1-acceptance.md`, retain
their original citations. Resolve those old root-relative names using this map:

| Original path | Current location |
| --- | --- |
| `GPUI_KIT_*` except the watch/polling record | [GPUI Kit archive](gpui-kit/), same filename; complete list below |
| `GPUI_KIT_WATCH_POLLING_MITIGATION.md` | [Watch and polling mitigation](../watch-polling-mitigation.md) |
| `AUTOMATIONS_IMPLEMENTATION_REPORT.md` | [Automations implementation report](automations/AUTOMATIONS_IMPLEMENTATION_REPORT.md) |
| `improvements.md` | [Idle-work performance report, 2026-10-12](performance/idle-work-2026-10-12.md) |
| `PERFORMANCE_RECOVERY_REPORT.txt` | [Performance recovery report](performance/PERFORMANCE_RECOVERY_REPORT.txt) |
| `inline-timeline-design-20261014.md` | [Inline timeline design](design/inline-timeline-design-20261014.md) |
| Root `WORKER_REPORT.md` duplicate summary | [Terminal integration report](../../crates/riwork-nsh-terminal/WORKER_REPORT.md) and [original Ghostty spike](../../spikes/ghostty-nsh/WORKER_REPORT.md) retain the evidence |

### GPUI Kit reports

Every filename below was previously at the repository root:

- [GPUI_KIT_BEHAVIOR_CONTRACT.md](gpui-kit/GPUI_KIT_BEHAVIOR_CONTRACT.md)
- [GPUI_KIT_CHAT_PLAN.md](gpui-kit/GPUI_KIT_CHAT_PLAN.md)
- [GPUI_KIT_CONTROLS_FOCUSED_REVIEW.md](gpui-kit/GPUI_KIT_CONTROLS_FOCUSED_REVIEW.md)
- [GPUI_KIT_CONTROLS_IMPLEMENTATION.md](gpui-kit/GPUI_KIT_CONTROLS_IMPLEMENTATION.md)
- [GPUI_KIT_CONTROLS_PROPOSED_TESTS.json](gpui-kit/GPUI_KIT_CONTROLS_PROPOSED_TESTS.json)
- [GPUI_KIT_CONTROL_INVENTORY.md](gpui-kit/GPUI_KIT_CONTROL_INVENTORY.md)
- [GPUI_KIT_DRAFT_IMAGE_PREVIEW_RECEIPT.md](gpui-kit/GPUI_KIT_DRAFT_IMAGE_PREVIEW_RECEIPT.md)
- [GPUI_KIT_FORMS_PLAN.md](gpui-kit/GPUI_KIT_FORMS_PLAN.md)
- [GPUI_KIT_FORMS_RECEIPT.md](gpui-kit/GPUI_KIT_FORMS_RECEIPT.md)
- [GPUI_KIT_FOUNDATION_CORRECTION.md](gpui-kit/GPUI_KIT_FOUNDATION_CORRECTION.md)
- [GPUI_KIT_INPUT_CONTRACT.md](gpui-kit/GPUI_KIT_INPUT_CONTRACT.md)
- [GPUI_KIT_PROJECT_SEARCH_PROPOSED_TESTS.json](gpui-kit/GPUI_KIT_PROJECT_SEARCH_PROPOSED_TESTS.json)
- [GPUI_KIT_PROJECT_SEARCH_RECEIPT.md](gpui-kit/GPUI_KIT_PROJECT_SEARCH_RECEIPT.md)
- [GPUI_KIT_TAB_CLOSE_CORRECTION.md](gpui-kit/GPUI_KIT_TAB_CLOSE_CORRECTION.md)
- [GPUI_KIT_TRANSCRIPT_SELECTION_CORRECTIONS.md](gpui-kit/GPUI_KIT_TRANSCRIPT_SELECTION_CORRECTIONS.md)
- [GPUI_KIT_TRANSCRIPT_SELECTION_FINAL_CORRECTIONS.md](gpui-kit/GPUI_KIT_TRANSCRIPT_SELECTION_FINAL_CORRECTIONS.md)
- [GPUI_KIT_TRANSCRIPT_SELECTION_FOLLOWUP.md](gpui-kit/GPUI_KIT_TRANSCRIPT_SELECTION_FOLLOWUP.md)
- [GPUI_KIT_TRANSCRIPT_SELECTION_REPORT.md](gpui-kit/GPUI_KIT_TRANSCRIPT_SELECTION_REPORT.md)
- [GPUI_KIT_WORKSPACE_INTEGRATION.md](gpui-kit/GPUI_KIT_WORKSPACE_INTEGRATION.md)

## NSH rollout records retained in place

The NSH evidence uses path-based manifests and checksums, including the
[prior-evidence manifest](../verification/nsh-schedules-parity-42978307/prior-evidence-sha256.json).
Reports, failed runs, empty logs and repeated environment captures keep their
original identities. Their original source paths and temporary tool locations
describe the recorded run, not a portable command for today's checkout.

| Topic | Reports and evidence |
| --- | --- |
| Phase 1 plan and initial acceptance | [Acceptance plan](../nsh-phase1-acceptance.md), [M5 results](../nsh-phase1-m5-results.md), [machine-readable results](../nsh-phase1-m5-results.json) |
| Phase 1 integration and consolidation | [Integration](../nsh-phase1-integration.md), [consolidation](../nsh-phase1-consolidation.md), [evidence](../verification/nsh-p1-consolidation/) |
| Workspace, host and native reruns | [W1 workspace](../nsh-phase1-w1-results.md), [W2 host](../nsh-phase1-w2-host-results.md), [W3 native](../nsh-phase1-w3-native-results.md), [panels merge](../nsh-phase1-w3-panels-merge.md), [native evidence](../verification/nsh-w3-native/), [merge evidence](../verification/nsh-w3-panels-merge/) |
| Panels and files | [Phase 3a](../nsh-phase3a-panels.md), [M5 merge](../nsh-phase3a-m5-merge.md), [Phase 3b](../nsh-phase3b-files.md), [memo-save race](../nsh-memo-save-race.md) |
| Chat and global window | [Phase 4a chat](../nsh-phase4a-chat.md), [Phase 5 global](../nsh-phase5-global.md), [chat image verification](../verification/nsh-chat-images-6d2cc795/README.md) |
| Settings, automations, remote and previews | [Phase 6a settings](../nsh-phase6a-settings.md), [Phase 6b](../nsh-phase6b-report.md), [6b evidence](../verification/nsh-phase6b/), [Phase 6c](../nsh-phase6c-cua-preview.md), [Phase 6 review](../nsh-phase6-review.md) |
| Schedules parity | [Follow-up](../nsh-schedules-parity-followup.md), [evidence interpretation](../verification/nsh-schedules-parity-42978307/README.md) |
| Terminal/preview lifecycle | [Ghostty teardown scope](../nsh-ghostty-teardown-scope.md), [native preview](../nsh-native-preview.md), [pending-open recovery](../nsh-pending-open-recovery.md), [quit-recovery ordering](../nsh-quit-recovery-ordering.md) |

The sibling `nsh-phase1-m5-*` and `nsh-phase1-w1-*` reports, JSON receipts and
screenshots provide the component evidence for those aggregate reports. Original
phase status remains historical: later reports may supersede particular results
without rewriting the earlier failures or closing every acceptance item.

## Other historical records retained in place

- [Protocol v2 verification](../remote-protocol-v2-verification.md) and [WSS provider correction](../remote-wss-provider.md).
- [Scheduling acceptance](../scheduling-verification.md) and [Automations artifact interpretation](../verification/automations/README.md).
- [Shared-tabs implementation](../shared-tabs-report.md) and [review corrections](../shared-tabs-review-report.md); the [shared-tabs contract](../shared-tabs.md) remains separate.
- [Native-theme performance](../performance-native-theme.md), its [frame log](../perf-native-theme-frames.log), and [tool UI performance](../tool-ui-performance.md).

The [active performance handover](../performance-audit-handover-2026-11-06.md)
and its pass reports are not completed historical work: pass11 remains
unfinished. Keep their manifests, source variants and `target/performance-audit/`
evidence intact. The [phone gateway guide](../phone-ssh-gateway.md) also remains
in current documentation; its implementation was built/tested and awaits deployment.
