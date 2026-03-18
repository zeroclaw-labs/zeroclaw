## Summary
- Fix route-specific `api_key` being dropped in Channel/Agent mode
- Extend `ChannelRouteSelection` to include `api_key` field
- Update `classify_message_route` to preserve `route.api_key` from `[[model_routes]]`
- Update `get_or_create_provider` to accept and use route-specific API key
- Fix cache poisoning by including API key hash in provider cache key

## Root Cause
When `query_classification` routed a message to a `[[model_routes]]` entry with a custom `api_key`:
1. `ChannelRouteSelection` struct only had `provider` and `model` fields, no `api_key`
2. `classify_message_route` discarded `route.api_key` when building `ChannelRouteSelection`
3. `get_or_create_provider` always used `defaults.api_key` (global) instead of route-specific key
4. Provider cache used only `provider_name` as key, causing cache poisoning when same provider was used with different api_keys

## Changes
1. **`ChannelRouteSelection`**: Added `api_key: Option<String>` field
2. **`classify_message_route`**: Now preserves `route.api_key.clone()`
3. **`get_or_create_provider`**: 
   - Added `route_api_key: Option<&str>` parameter
   - Cache key now includes API key hash to prevent poisoning
   - Uses route-specific key when available, falls back to global
4. **Call sites**: Updated to pass route API key

## Security Note
The provider cache key now includes a hash of the API key to ensure routes with different credentials get separate provider instances. This prevents credential leakage across routes.

## Validation
- `cargo check -p zeroclaw --lib` passes
- `cargo fmt --all -- --check` passes

Closes #3838
