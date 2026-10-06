# Focused source correction receipt

Project `39832c2e-23a5-476d-aa8f-5ff34a02d314`, task
`bf157564-29a2-470b-abc3-ffe1dde412cc`, worktree
`/Users/dominik/orca/projects/riWork-feat-gpui-kit-chat-input`, branch
`feat/gpui-kit-transcript-selection`. Verified starting HEAD
`97a269d75b40f3a6d35c28bc310ab04ae2223da5`. No new features or foundation edits.
Prior reports and the untracked rollout audit are preserved unchanged.

## Incorporated parent corrections

Read the reviewed integration commits and manually applied only owned hunks:

- `8e4ce14eeebea48618261ceeaf5d5638e4f0e22f`: explicit MouseDownEvent type,
  Focusable import, pinned `Role::Label` in transcript source and assertion.
  Retained the current modal-aware foundation scope call.
- `6c1f5da944de019d550affeebd14a874853a44c4`: the nonexistent QuestionPrompt ID
  had already been removed in 97a269d; retained that correction. No wire change.
- `6192c948178c3132117dc51f077c009c24047577`: synthetic messages now have stable
  per-message turn IDs and explicit Final phase so default Normal renders them.
  Did not switch all tests to Verbose or relax missing-element assertions.
- `54bf29cb2d62da240a523832acb84e79cebe6bfe`: menu-equivalent Copy assertions
  now occur after the borrowed window update ends and run_until_parked drains
  deferred dispatch. Applied this timing correction to both new menu checks
  in the virtualized partial-selection and whitespace fixtures as well.
  Empty focused-editor Copy expects the unchanged clipboard sentinel; asserts
  that the composer is focused and its selected range is collapsed.

Read the reviewer's early receipt at GPUI_KIT_REVIEW_REPORT.md:998. Those early
passes belong to a different immutable participant implementation and are not
validation of this document implementation or its new fixtures.

## Actual AX-node lifecycle correction

`chat_shared_widgets_actual_ax_nodes_keep_names_states_and_disabled_click_capability`
no longer calls Control.render from update_window. Its minimal mounted fixture
has one Base Root, in-memory settings/appearance and recording Feed only; no
Workspace, ChatView/HostConfig, SessionManager, speech or service constructor.

GPUI invokes the observation component's RenderOnce from request_layout. The
transparent element delegates identity, layout, prepaint, paint and AX methods
to the actual shared control. After that control prepaints, it captures its
actual AX node and nonzero layout bounds, including foundation's disabled-state
refinement. All eight button/toggle/appearance/enabled combinations must have
real captured nodes. The same strict role/name/pressed/disabled/Click-capability
assertions remain. No mock metadata, substitute hitbox or activation engine.
This remains native-node metadata evidence, not OS AX action-delivery proof.

## Retained fixture scope and limits

The virtualized partial-selection fixture still uses real held pointer/wheel
events in both directions, proves endpoint and intervening element/geometry
unmount, checks exact partial Unicode Copy after both endpoints disappear,
preserves selection across unrelated tail replacement and invalidates a touched
unmounted middle row before paint. Its deferred menu Copy completes before
those subsequent mutations. Whitespace-only/code blank-line/trailing-space
expectations and editor-to-transcript focus/draft checks remain exact.

Standalone source formatting and Git whitespace inspection only; **zero Cargo,
build/check/test/test-list, fixture/runtime/provider/service/socket/speech,
process/session/tmux/native GUI or production-state activity** by this author.
No branch switch, cherry-pick, merge/push, install/reload or other-worker edit.

The reviewer must compile the latest immutable assembly with foundation's
content/role/scope API (d92cbb9 and its owner corrections), then run these actual
selection/editor/control fixtures in its audited private environment. All
current fixture outcomes remain pending. Parent retains exclusive Cua.ai Driver
MCP ownership for native acceptance. This is a focused source milestone only.
