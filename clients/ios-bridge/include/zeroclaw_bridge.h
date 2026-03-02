/*
 * ZeroClaw iOS Bridge — C Header for Swift Bridging
 *
 * Include this header in your Xcode project's Bridging Header:
 *   #import "zeroclaw_bridge.h"
 *
 * Link against libzeroclaw_ios.a (built with cargo-lipo for universal binary).
 *
 * Build targets:
 *   aarch64-apple-ios         (iPhone ARM64)
 *   aarch64-apple-ios-sim     (Simulator Apple Silicon)
 *   x86_64-apple-ios          (Simulator Intel)
 */

#ifndef ZEROCLAW_BRIDGE_H
#define ZEROCLAW_BRIDGE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Start the ZeroClaw gateway in-process.
 *
 * @param data_dir  Path to the data directory (app sandbox Documents path).
 * @param provider  LLM provider name ("gemini", "openai", "anthropic").
 * @param api_key   API key for the provider (NULL if using operator key).
 * @param port      Gateway port (e.g. 3000).
 * @return 0 on success, -1 on error.
 */
int32_t zeroclaw_start(const char *data_dir,
                       const char *provider,
                       const char *api_key,
                       uint16_t port);

/**
 * Send a message to the ZeroClaw agent.
 *
 * @param message  User message (UTF-8 C string).
 * @return Newly allocated response string. Caller MUST free with zeroclaw_free_string().
 *         Returns NULL on error.
 */
char *zeroclaw_send_message(const char *message);

/**
 * Get the current status of the ZeroClaw agent.
 *
 * @return 1 if running, 0 if stopped, -1 on error.
 */
int32_t zeroclaw_get_status(void);

/**
 * Stop the ZeroClaw gateway gracefully.
 */
void zeroclaw_stop(void);

/**
 * Free a string returned by zeroclaw_send_message.
 *
 * @param ptr  Pointer returned by zeroclaw_send_message. Safe to pass NULL.
 */
void zeroclaw_free_string(char *ptr);

/**
 * Set the authentication token for gateway communication.
 *
 * @param token  Bearer token (UTF-8 C string). Pass NULL to clear.
 */
void zeroclaw_set_token(const char *token);

#ifdef __cplusplus
}
#endif

#endif /* ZEROCLAW_BRIDGE_H */
