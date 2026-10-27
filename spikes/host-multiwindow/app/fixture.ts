// Matches chat_view/display_tests.rs::long_answer_streaming_costs exactly.
// Generated local text only: no real conversations or network requests.
export const TRANSCRIPT = Array.from({ length: 60 }, (_, n) =>
  `## Part ${n}\n\nThe fold keeps its order because \`apply\` sorts by sequence, and ` +
  `**turn ${n}** reads [the parser](src/parse.rs:${n}) before it writes. Nothing ` +
  `else changed in src/lib.rs, so the tests still pass.\n\n` +
  `- first point about the change\n- second point\n  - nested detail\n\n` +
  `| file | lines |\n|---|---|\n| src/lib.rs | ${n} |\n\n`
).join("");

export const FIXTURE_BYTES = 18_980;
export const FIXTURE_CHUNKS = 475;
export const DEFAULT_CHUNK_BYTES = 40;
