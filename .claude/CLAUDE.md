# manage_our_home

## Development process

- Write unit tests before implementing new logic (TDD): for any new pure-logic
  function (validation, permission checks, formatting, parsing, etc.), write
  the `#[cfg(test)] mod tests` cases first, watch them fail, then implement.
  See `apps/api/src/messagerie/messages.rs` (`validate_content`) or
  `apps/api/src/messagerie/mod.rs` (`can_modify`) for the expected shape.
- Integration/flow tests (`apps/api/tests/*_flow.rs`) still cover end-to-end
  behavior against a real DB; unit tests are for the pure logic pulled out of
  handlers, not a replacement for them.
