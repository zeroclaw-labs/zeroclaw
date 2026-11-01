> Historical compatibility-worker evidence: this records the revision named below.
> The current transport is native Rust. Current reproduction steps are in
> `crates/zeroclaw-channels/tests/xmpp_native/README.md`; the retired Python
> bridge runners and `ZEROCLAW_XMPP_PYTHON` are not current runtime setup.

# Bidirectional pipe backpressure regression

Review finding: [bidirectional pipe deadlock](https://github.com/zeroclaw-labs/zeroclaw/pull/11847#discussion_r4355334195)

The listener previously awaited complete stdin writes inside its select branch, so it could not drain stdout while a large outgoing frame was blocked. The Python worker also wrote stdout synchronously on its event loop.

The adapter now polls a bounded stdin writer alongside stdout frames. No detached writer task outlives the listener; dropping the listener drops the writer and the kill-on-drop worker. Python emits serialized UTF-8 frames through a thread, keeping blocking pipe writes off the XMPP event loop. TLS, authorization, OMEMO and frame bounds are unchanged.

The Unix Rust regression runs the actual Channel::listen and Channel::send paths against a disposable protocol worker. After online, the worker fills stdout with 64,000 non-BMP characters before reading a similarly sized outbound command (about 256 KiB in each direction). The listener must deliver the incoming message and receive the send acknowledgment within five seconds.

On the previous public listener at 8c47216e3 with this new test appended: authorization/offline tests passed, but the full-pipe test failed at the five-second timeout. With the correction: all three Rust XMPP tests passed; the full-pipe exchange completed in the suite's 0.12 seconds. This proves the regression distinguishes the old blocked implementation from the repair.

Four Python tests passed, including a blocking stdout fixture that still permits another event-loop task to run. Maximum Unicode/control frame round trips and private SQLite behavior remain covered.

The protocol worker is deliberately a local pipe fixture, not a TLS/OMEMO server test. Earlier assembled Prosody listener proof and TLS/OMEMO transport evidence remain complementary. Fresh upstream CI and the integrated production candidate are separate checks; this report does not claim deployment.
